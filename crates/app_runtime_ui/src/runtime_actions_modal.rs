use app_runtime::{
    CapabilityState, ExecutionRequest, RuntimeAction, RuntimeCatalog, RuntimeError,
    SystemCommandRunner,
};
use gpui::{
    App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, Render, Task, WeakEntity,
};
use ui::{
    Button, ButtonSize, ButtonStyle, Color, ContextMenu, ContextMenuEntry, DropdownMenu,
    DropdownStyle, IconPosition, Label, LabelSize, Modal, ModalFooter, ModalHeader, prelude::*,
};
use workspace::notifications::NotificationId;
use workspace::{ModalView, Toast, Workspace};

use crate::OpenRuntimeActions;
use crate::runtime_execution::execute_runtime_request;

pub struct RuntimeActionsModal {
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
    catalog: Option<RuntimeCatalog>,
    selection: RuntimeSelectionState,
    loading: bool,
    _loading_task: Option<Task<()>>,
}

impl RuntimeActionsModal {
    pub fn toggle(
        workspace: &mut Workspace,
        _: &OpenRuntimeActions,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let workspace_paths = workspace
            .project()
            .read(cx)
            .visible_worktrees(cx)
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
            .collect::<Vec<_>>();
        let workspace_handle = workspace.weak_handle();

        workspace.toggle_modal(window, cx, |window, cx| {
            Self::new(workspace_handle.clone(), workspace_paths.clone(), window, cx)
        });
    }

    fn new(
        workspace: WeakEntity<Workspace>,
        workspace_paths: Vec<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let load_task = cx.spawn_in(window, async move |this, cx| {
            let catalog = cx
                .background_spawn(async move {
                    let runner = SystemCommandRunner;
                    RuntimeCatalog::discover(&workspace_paths, &runner)
                })
                .await;

            this.update_in(cx, |this, _window, cx| {
                this.loading = false;
                this.selection = choose_initial_selection(&catalog);
                this.catalog = Some(catalog);
                cx.notify();
            })
            .ok();
        });

        Self {
            focus_handle,
            workspace,
            catalog: None,
            selection: RuntimeSelectionState::default(),
            loading: true,
            _loading_task: Some(load_task),
        }
    }

    fn selected_project(&self) -> Option<&app_runtime::DetectedProject> {
        let catalog = self.catalog.as_ref()?;
        selected_project(catalog, &self.selection)
    }

    fn selected_target(&self) -> Option<&app_runtime::RuntimeTarget> {
        let project = self.selected_project()?;
        project
            .targets
            .iter()
            .find(|target| Some(&target.id) == self.selection.target_id.as_ref())
    }

    fn selected_device(&self) -> Option<&app_runtime::RuntimeDevice> {
        let project = self.selected_project()?;
        project
            .devices
            .iter()
            .find(|device| Some(&device.id) == self.selection.device_id.as_ref())
    }

    fn select_project(&mut self, project_id: String, cx: &mut Context<Self>) {
        if let Some(catalog) = self.catalog.as_ref() {
            select_project(catalog, &mut self.selection, project_id);
            cx.notify();
        }
    }

    fn select_target(&mut self, target_id: String, cx: &mut Context<Self>) {
        if let Some(project) = self.selected_project().cloned() {
            select_target(&project, &mut self.selection, target_id);
            cx.notify();
        }
    }

    fn select_device(&mut self, device_id: String, cx: &mut Context<Self>) {
        self.selection.device_id = Some(device_id);
        cx.notify();
    }

    fn action_request(&self, action: RuntimeAction) -> Result<ExecutionRequest, RuntimeError> {
        let project = self
            .selected_project()
            .ok_or_else(|| RuntimeError::ProjectNotFound("selected-project".to_string()))?;
        let target = self
            .selected_target()
            .ok_or_else(|| RuntimeError::TargetNotFound("selected-target".to_string()))?;

        Ok(ExecutionRequest {
            project_id: project.id.clone(),
            target_id: target.id.clone(),
            device_id: match action {
                RuntimeAction::Build => None,
                RuntimeAction::Run => self.selected_device().map(|device| device.id.clone()),
            },
            action,
        })
    }

    fn action_reason(&self, action: RuntimeAction) -> Option<String> {
        let project = self.selected_project()?;
        if self.selected_target().is_none() {
            return Some("Choose a target.".to_string());
        }

        let capability = match action {
            RuntimeAction::Build => &project.capabilities.build,
            RuntimeAction::Run => &project.capabilities.run,
        };

        match capability {
            CapabilityState::Available => {
                if matches!(action, RuntimeAction::Run) && self.selected_device().is_none() {
                    Some("Choose a device.".to_string())
                } else {
                    None
                }
            }
            CapabilityState::RequiresSetup { reason }
            | CapabilityState::Unavailable { reason } => Some(reason.clone()),
        }
    }

    fn run_action(&mut self, action: RuntimeAction, window: &mut Window, cx: &mut Context<Self>) {
        if self.action_reason(action).is_some() {
            return;
        }

        let request = match self.action_request(action) {
            Ok(request) => request,
            Err(error) => {
                self.show_error(error.to_string(), cx);
                return;
            }
        };
        let Some(catalog) = self.catalog.as_ref() else {
            return;
        };

        let launch_result = self
            .workspace
            .update(cx, |workspace, cx| {
                execute_runtime_request(workspace, catalog, &request, window, cx)
            })
            .ok()
            .and_then(Result::ok);

        match launch_result {
            Some(()) => cx.emit(DismissEvent),
            None => self.show_error("Could not start the runtime action.", cx),
        }
    }

    fn show_error(&self, message: impl Into<String>, cx: &mut Context<Self>) {
        let message = message.into();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_toast(
                    Toast::new(NotificationId::unique::<RuntimeActionsModal>(), message)
                        .autohide(),
                    cx,
                );
            })
            .ok();
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn render_project_dropdown(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label = self
            .selected_project()
            .map(|project| project.label.clone())
            .unwrap_or_else(|| "Select project".to_string());
        let modal = cx.entity().downgrade();

        let menu = ContextMenu::build(window, cx, {
            let projects = self
                .catalog
                .as_ref()
                .map(|catalog| catalog.projects.clone())
                .unwrap_or_default();
            let selected_project_id = self.selection.project_id.clone();
            let menu_modal = modal.clone();
            move |mut menu, _, _| {
                for project in &projects {
                    let is_selected = selected_project_id.as_ref() == Some(&project.id);
                    let project_id = project.id.clone();
                    let modal = menu_modal.clone();
                    menu.push_item(
                        ContextMenuEntry::new(project.label.clone())
                            .toggleable(IconPosition::End, is_selected)
                            .handler(move |_, cx| {
                                modal.update(cx, |this, cx| {
                                    this.select_project(project_id.clone(), cx);
                                }).ok();
                            }),
                    );
                }
                menu
            }
        });

        DropdownMenu::new("runtime-project-selector", label, menu)
            .style(DropdownStyle::Outlined)
            .trigger_size(ButtonSize::Large)
            .full_width(true)
            .disabled(self.loading || self.catalog.as_ref().is_none_or(|catalog| catalog.projects.is_empty()))
    }

    fn render_target_dropdown(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label = self
            .selected_target()
            .map(|target| target.label.clone())
            .unwrap_or_else(|| "Select target".to_string());
        let modal = cx.entity().downgrade();

        let targets = self
            .selected_project()
            .map(|project| project.targets.clone())
            .unwrap_or_default();
        let has_targets = !targets.is_empty();
        let selected_target_id = self.selection.target_id.clone();
        let menu_modal = modal.clone();
        let menu = ContextMenu::build(window, cx, move |mut menu, _, _| {
            for target in &targets {
                let is_selected = selected_target_id.as_ref() == Some(&target.id);
                let target_id = target.id.clone();
                let modal = menu_modal.clone();
                menu.push_item(
                    ContextMenuEntry::new(target.label.clone())
                        .toggleable(IconPosition::End, is_selected)
                        .handler(move |_, cx| {
                            modal.update(cx, |this, cx| {
                                this.select_target(target_id.clone(), cx);
                            }).ok();
                        }),
                );
            }
            menu
        });

        DropdownMenu::new("runtime-target-selector", label, menu)
            .style(DropdownStyle::Outlined)
            .trigger_size(ButtonSize::Large)
            .full_width(true)
            .disabled(self.loading || !has_targets)
    }

    fn render_device_dropdown(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label = self
            .selected_device()
            .map(|device| {
                device
                    .os_version
                    .as_ref()
                    .map(|os_version| format!("{} ({os_version})", device.name))
                    .unwrap_or_else(|| device.name.clone())
            })
            .unwrap_or_else(|| "Select device".to_string());
        let modal = cx.entity().downgrade();

        let devices = self
            .selected_project()
            .map(|project| project.devices.clone())
            .unwrap_or_default();
        let has_devices = !devices.is_empty();
        let selected_device_id = self.selection.device_id.clone();
        let menu_modal = modal.clone();
        let menu = ContextMenu::build(window, cx, move |mut menu, _, _| {
            for device in &devices {
                let item_label = device
                    .os_version
                    .as_ref()
                    .map(|os_version| format!("{} ({os_version})", device.name))
                    .unwrap_or_else(|| device.name.clone());
                let is_selected = selected_device_id.as_ref() == Some(&device.id);
                let device_id = device.id.clone();
                let modal = menu_modal.clone();
                menu.push_item(
                    ContextMenuEntry::new(item_label)
                        .toggleable(IconPosition::End, is_selected)
                        .handler(move |_, cx| {
                            modal.update(cx, |this, cx| {
                                this.select_device(device_id.clone(), cx);
                            }).ok();
                        }),
                );
            }
            menu
        });

        DropdownMenu::new("runtime-device-selector", label, menu)
            .style(DropdownStyle::Outlined)
            .trigger_size(ButtonSize::Large)
            .full_width(true)
            .disabled(self.loading || !has_devices)
    }

    fn render_selection_details(&self, cx: &mut Context<Self>) -> impl IntoElement {
        match self.selected_project() {
            None if self.loading => Label::new("Detecting runtime capabilities…")
                .size(LabelSize::Small)
                .color(Color::Muted)
                .into_any_element(),
            None => Label::new("No runnable project was detected in this workspace.")
                .size(LabelSize::Small)
                .color(Color::Muted)
                .into_any_element(),
            Some(project) => v_flex()
                .gap_1()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(cx.theme().colors().border)
                .bg(cx.theme().colors().background)
                .child(Label::new(project.label.clone()))
                .child(
                    Label::new(format!(
                        "{} · {}",
                        project_kind_label(project.kind.clone()),
                        project.workspace_root.display()
                    ))
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .single_line(),
                )
                .child(
                    Label::new(capability_line("Build", &project.capabilities.build))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .child(
                    Label::new(capability_line("Run", &project.capabilities.run))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element(),
        }
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let build_reason = self.action_reason(RuntimeAction::Build);
        let run_reason = self.action_reason(RuntimeAction::Run);
        let status_message = run_reason
            .clone()
            .or(build_reason.clone())
            .unwrap_or_else(|| "Ready to build or run.".to_string());

        h_flex()
            .justify_between()
            .items_center()
            .gap_3()
            .child(
                Label::new(status_message)
                    .size(LabelSize::Small)
                    .color(if run_reason.is_some() || build_reason.is_some() {
                        Color::Muted
                    } else {
                        Color::Success
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("runtime-build", "Build")
                            .style(ButtonStyle::Outlined)
                            .disabled(self.loading || build_reason.is_some())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.run_action(RuntimeAction::Build, window, cx);
                            })),
                    )
                    .child(
                        Button::new("runtime-run", "Run")
                            .style(ButtonStyle::Filled)
                            .disabled(self.loading || run_reason.is_some())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.run_action(RuntimeAction::Run, window, cx);
                            })),
                    ),
            )
    }
}

impl EventEmitter<DismissEvent> for RuntimeActionsModal {}

impl Focusable for RuntimeActionsModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ModalView for RuntimeActionsModal {}

impl Render for RuntimeActionsModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("RuntimeActionsModal")
            .occlude()
            .elevation_3(cx)
            .w(rems(42.))
            .on_action(cx.listener(Self::cancel))
            .track_focus(&self.focus_handle)
            .child(
                Modal::new("runtime-actions-modal", None::<gpui::ScrollHandle>)
                    .header(
                        ModalHeader::new()
                            .headline("Runtime")
                            .description("Pick a project, target, and device.")
                            .show_dismiss_button(true),
                    )
                    .child(
                        v_flex()
                            .gap_3()
                            .p_3()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_start()
                                    .child(
                                        v_flex()
                                            .gap_1()
                                            .flex_1()
                                            .child(
                                                Label::new("Project")
                                                    .size(LabelSize::Small)
                                                    .color(Color::Muted),
                                            )
                                            .child(self.render_project_dropdown(window, cx)),
                                    )
                                    .child(
                                        v_flex()
                                            .gap_1()
                                            .flex_1()
                                            .child(
                                                Label::new("Target")
                                                    .size(LabelSize::Small)
                                                    .color(Color::Muted),
                                            )
                                            .child(self.render_target_dropdown(window, cx)),
                                    )
                                    .child(
                                        v_flex()
                                            .gap_1()
                                            .flex_1()
                                            .child(
                                                Label::new("Device")
                                                    .size(LabelSize::Small)
                                                    .color(Color::Muted),
                                            )
                                            .child(self.render_device_dropdown(window, cx)),
                                    ),
                            )
                            .child(self.render_selection_details(cx)),
                    )
                    .footer(ModalFooter::new().end_slot(self.render_footer(cx))),
            )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RuntimeSelectionState {
    project_id: Option<String>,
    target_id: Option<String>,
    device_id: Option<String>,
}

fn choose_initial_selection(catalog: &RuntimeCatalog) -> RuntimeSelectionState {
    catalog
        .projects
        .first()
        .map(selection_for_project)
        .unwrap_or_default()
}

fn selection_for_project(project: &app_runtime::DetectedProject) -> RuntimeSelectionState {
    RuntimeSelectionState {
        project_id: Some(project.id.clone()),
        target_id: project.targets.first().map(|target| target.id.clone()),
        device_id: project.devices.first().map(|device| device.id.clone()),
    }
}

fn selected_project<'a>(
    catalog: &'a RuntimeCatalog,
    selection: &RuntimeSelectionState,
) -> Option<&'a app_runtime::DetectedProject> {
    selection
        .project_id
        .as_ref()
        .and_then(|project_id| catalog.projects.iter().find(|project| &project.id == project_id))
}

fn select_project(
    catalog: &RuntimeCatalog,
    selection: &mut RuntimeSelectionState,
    project_id: String,
) {
    if let Some(project) = catalog.projects.iter().find(|project| project.id == project_id) {
        *selection = selection_for_project(project);
    }
}

fn select_target(
    project: &app_runtime::DetectedProject,
    selection: &mut RuntimeSelectionState,
    target_id: String,
) {
    if project.targets.iter().any(|target| target.id == target_id) {
        selection.target_id = Some(target_id);
        if selection.device_id.is_none() && !project.devices.is_empty() {
            selection.device_id = project.devices.first().map(|device| device.id.clone());
        }
    }
}

fn capability_line(label: &str, capability: &CapabilityState) -> String {
    match capability {
        CapabilityState::Available => format!("{label}: available"),
        CapabilityState::RequiresSetup { reason } | CapabilityState::Unavailable { reason } => {
            format!("{label}: {reason}")
        }
    }
}

fn project_kind_label(kind: app_runtime::ProjectKind) -> &'static str {
    match kind {
        app_runtime::ProjectKind::AppleWorkspace => "Apple workspace",
        app_runtime::ProjectKind::AppleProject => "Apple project",
        app_runtime::ProjectKind::GpuiApplication => "GPUI app",
    }
}

#[cfg(test)]
mod tests {
    use app_runtime::{
        CapabilityState, DetectedProject, ProjectKind, RuntimeCapabilitySet, RuntimeDevice,
        RuntimeDeviceKind, RuntimeDeviceState, RuntimeTarget,
    };

    use super::{
        RuntimeSelectionState, choose_initial_selection, select_project, select_target,
        selection_for_project,
    };

    fn project(id: &str, targets: &[&str], devices: &[&str]) -> DetectedProject {
        DetectedProject {
            id: id.to_string(),
            label: id.to_string(),
            kind: ProjectKind::GpuiApplication,
            workspace_root: std::path::PathBuf::from(format!("/tmp/{id}")),
            project_path: std::path::PathBuf::from(format!("/tmp/{id}/Cargo.toml")),
            targets: targets
                .iter()
                .map(|target| RuntimeTarget {
                    id: (*target).to_string(),
                    label: (*target).to_string(),
                })
                .collect(),
            devices: devices
                .iter()
                .map(|device| RuntimeDevice {
                    id: (*device).to_string(),
                    name: (*device).to_string(),
                    kind: RuntimeDeviceKind::Desktop,
                    state: RuntimeDeviceState::Unknown,
                    os_version: None,
                })
                .collect(),
            capabilities: RuntimeCapabilitySet {
                run: CapabilityState::Available,
                build: CapabilityState::Available,
            },
        }
    }

    #[test]
    fn chooses_initial_selection_from_first_project() {
        let catalog = app_runtime::RuntimeCatalog {
            projects: vec![project("alpha", &["app"], &["mac"]), project("beta", &["tool"], &[])],
        };

        let selection = choose_initial_selection(&catalog);

        assert_eq!(
            selection,
            RuntimeSelectionState {
                project_id: Some("alpha".to_string()),
                target_id: Some("app".to_string()),
                device_id: Some("mac".to_string()),
            }
        );
    }

    #[test]
    fn resets_target_and_device_when_project_changes() {
        let catalog = app_runtime::RuntimeCatalog {
            projects: vec![project("alpha", &["app"], &["mac"]), project("beta", &["tool"], &[])],
        };
        let mut selection = selection_for_project(&catalog.projects[0]);

        select_project(&catalog, &mut selection, "beta".to_string());

        assert_eq!(selection.project_id.as_deref(), Some("beta"));
        assert_eq!(selection.target_id.as_deref(), Some("tool"));
        assert!(selection.device_id.is_none());
    }

    #[test]
    fn keeps_first_device_available_when_target_changes() {
        let project = project("alpha", &["app", "worker"], &["mac"]);
        let mut selection = selection_for_project(&project);
        selection.device_id = None;

        select_target(&project, &mut selection, "worker".to_string());

        assert_eq!(selection.target_id.as_deref(), Some("worker"));
        assert_eq!(selection.device_id.as_deref(), Some("mac"));
    }
}
