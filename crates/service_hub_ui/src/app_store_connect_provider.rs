use std::collections::BTreeMap;

use anyhow::Result;
use gpui::{App, Context, ScrollHandle, SharedString, Window};
use project::DirectoryLister;
use serde::Deserialize;
use service_hub::{
    ServiceOperationRequest, ServiceProviderDescriptor, ServiceResourceRef, ServiceRunDescriptor,
    ServiceRunState, ServiceWorkflowDescriptor, ServiceWorkflowKind, ServiceWorkflowRequest,
};
use ui::{
    AnyElement, Button, ButtonSize, ButtonStyle, Color, IconButton, IconName, Indicator, Label,
    LabelSize, Severity, WithScrollbar, h_flex, prelude::*, v_flex,
};

use crate::{
    app_store_connect_auth::{AscAuthSummary, load_auth_status},
    command_runner::{run_auth_action, run_json_operation, run_workflow},
    service_auth::{
        ServiceAuthFormState, ServiceAuthStatusSummary, ServiceAuthUiAction, ServiceAuthUiModel,
    },
    service_workflow::{
        ServiceWorkflowFormState, ServiceWorkflowOption, ServiceWorkflowRunSummary,
        ServiceWorkflowUiAction, ServiceWorkflowUiModel,
    },
    services_page::ServicesPage,
    services_provider::{
        ServiceResourceMenuEntry, ServiceResourceMenuModel, ServiceWorkspaceAdapter,
        ServicesPageState,
    },
};

pub(crate) const APP_STORE_CONNECT_PROVIDER_ID: &str = "app-store-connect";
const ASC_BUILDS_PAGE_SIZE: usize = 50;

pub(crate) fn build_app_store_connect_workspace_adapter(
    descriptor: ServiceProviderDescriptor,
    window: &mut Window,
    cx: &mut App,
) -> Option<Box<dyn ServiceWorkspaceAdapter>> {
    (descriptor.id == APP_STORE_CONNECT_PROVIDER_ID).then(|| {
        Box::new(AppStoreConnectWorkspaceProvider::new(
            descriptor, window, cx,
        )) as Box<dyn ServiceWorkspaceAdapter>
    })
}

fn with_app_store_connect_provider_mut<R>(
    page: &mut ServicesPage,
    callback: impl FnOnce(&mut AppStoreConnectWorkspaceProvider, &mut ServicesPageState) -> R,
) -> Option<R> {
    page.with_provider_mut(APP_STORE_CONNECT_PROVIDER_ID, |pane, state| {
        pane.as_any_mut()
            .downcast_mut::<AppStoreConnectWorkspaceProvider>()
            .map(|provider| callback(provider, state))
    })
    .flatten()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AscAppSummary {
    id: String,
    name: String,
    bundle_id: String,
    sku: String,
    primary_locale: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AscBuildSummary {
    id: String,
    build_number: String,
    marketing_version: Option<String>,
    platform: Option<String>,
    processing_state: String,
    uploaded_date: String,
    expiration_date: Option<String>,
    testflight_internal_state: Option<String>,
    testflight_external_state: Option<String>,
    app_store_version_id: Option<String>,
    app_store_state: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct AscBuildListState {
    builds: Vec<AscBuildSummary>,
    next_page_url: Option<String>,
    is_loading_more: bool,
    load_more_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AscBuildPage {
    builds: Vec<AscBuildSummary>,
    next_page_url: Option<String>,
}

#[derive(Clone, Debug)]
enum LoadState<T> {
    Loading,
    Ready(T),
    Error(String),
}

pub(crate) struct AppStoreConnectWorkspaceProvider {
    descriptor: ServiceProviderDescriptor,
    auth_form: ServiceAuthFormState,
    workflow_forms: BTreeMap<String, ServiceWorkflowFormState>,
    latest_run: Option<ServiceRunDescriptor>,
    auth_state: LoadState<AscAuthSummary>,
    apps_state: LoadState<Vec<AscAppSummary>>,
    builds_state: LoadState<AscBuildListState>,
    builds_scroll_handle: ScrollHandle,
}

impl AppStoreConnectWorkspaceProvider {
    // Failure modes:
    // - Authentication checks fail or return partial data.
    // - App listing fails or returns no apps, leaving the shell without a resource selection.
    // - A selected app disappears between refreshes and the shell must recover cleanly.
    // - Build loading fails independently from auth or app loading.
    pub fn new(descriptor: ServiceProviderDescriptor, window: &mut Window, cx: &mut App) -> Self {
        Self {
            auth_form: ServiceAuthFormState::new(&descriptor, window, cx),
            workflow_forms: descriptor
                .workflows
                .iter()
                .map(|workflow| {
                    (
                        workflow.id.clone(),
                        ServiceWorkflowFormState::new(workflow, window, cx),
                    )
                })
                .collect(),
            latest_run: None,
            descriptor,
            auth_state: LoadState::Loading,
            apps_state: LoadState::Loading,
            builds_state: LoadState::Ready(AscBuildListState::default()),
            builds_scroll_handle: ScrollHandle::new(),
        }
    }

    pub fn descriptor(&self) -> &ServiceProviderDescriptor {
        &self.descriptor
    }

    pub fn normalize_state(&self, state: &mut ServicesPageState) {
        if !self
            .descriptor
            .shell
            .navigation_items
            .iter()
            .any(|item| item.id == state.navigation_id)
        {
            state.navigation_id = self.descriptor.shell.default_navigation_item_id.clone();
        }

        if let LoadState::Ready(apps) = &self.apps_state {
            if !apps
                .iter()
                .any(|app| Some(app.id.as_str()) == state.selected_resource_id.as_deref())
            {
                state.selected_resource_id = apps.first().map(|app| app.id.clone());
            }
        }

        self.normalize_workflow_state(state);
    }

    pub fn refresh(
        &mut self,
        _state: &mut ServicesPageState,
        window: &mut Window,
        cx: &mut Context<ServicesPage>,
    ) {
        self.auth_state = LoadState::Loading;
        self.apps_state = LoadState::Loading;
        self.builds_state = LoadState::Ready(AscBuildListState::default());
        self.latest_run = None;
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let (auth_result, apps_result) = cx
                .background_spawn(async move {
                    let auth = load_auth_status().await;
                    let apps = load_apps().await;
                    (auth, apps)
                })
                .await;

            let selected_app_id = this
                .update_in(cx, |page, _window, cx| {
                    with_app_store_connect_provider_mut(page, |pane, state| {
                        pane.auth_state = match auth_result {
                            Ok(summary) => LoadState::Ready(summary),
                            Err(error) => LoadState::Error(error.to_string()),
                        };

                        match apps_result {
                            Ok(apps) => {
                                let next_selected_app_id = state
                                    .selected_resource_id
                                    .as_ref()
                                    .and_then(|selected_id| {
                                        apps.iter()
                                            .find(|app| &app.id == selected_id)
                                            .map(|app| app.id.clone())
                                    })
                                    .or_else(|| apps.first().map(|app| app.id.clone()));

                                pane.apps_state = LoadState::Ready(apps);
                                state.selected_resource_id = next_selected_app_id.clone();
                                pane.builds_state = if next_selected_app_id.is_some() {
                                    LoadState::Loading
                                } else {
                                    LoadState::Ready(AscBuildListState::default())
                                };
                                cx.notify();
                                next_selected_app_id
                            }
                            Err(error) => {
                                pane.apps_state = LoadState::Error(error.to_string());
                                state.selected_resource_id = None;
                                pane.builds_state = LoadState::Ready(AscBuildListState::default());
                                cx.notify();
                                None
                            }
                        }
                    })
                })
                .ok()
                .flatten()
                .flatten();

            if let Some(app_id) = selected_app_id {
                this.update_in(cx, |page, window, cx| {
                    with_app_store_connect_provider_mut(page, |pane, state| {
                        pane.load_builds_for_app(state, app_id, window, cx);
                    });
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn resource_menu(&self, state: &ServicesPageState) -> Option<ServiceResourceMenuModel> {
        let resource_kind = self.descriptor.shell.resource_kind.as_ref()?;
        let current_label = match &self.apps_state {
            LoadState::Loading => format!("Loading {}…", resource_kind.plural_label),
            LoadState::Error(_) => format!("Select {}", resource_kind.singular_label),
            LoadState::Ready(apps) if apps.is_empty() => {
                format!("No {}", resource_kind.plural_label)
            }
            LoadState::Ready(apps) => state
                .selected_resource_id
                .as_ref()
                .and_then(|selected_id| apps.iter().find(|app| &app.id == selected_id))
                .map(|app| app.name.clone())
                .unwrap_or_else(|| format!("Select {}", resource_kind.singular_label)),
        };

        Some(ServiceResourceMenuModel {
            singular_label: resource_kind.singular_label.clone(),
            current_label,
            entries: self
                .apps()
                .iter()
                .map(|app| ServiceResourceMenuEntry {
                    id: app.id.clone(),
                    label: app.name.clone(),
                    detail: Some(app.bundle_id.clone()),
                })
                .collect(),
            disabled: matches!(self.apps_state, LoadState::Loading) || self.apps().is_empty(),
        })
    }

    pub fn select_resource(
        &mut self,
        state: &mut ServicesPageState,
        resource_id: String,
        window: &mut Window,
        cx: &mut Context<ServicesPage>,
    ) {
        if state.selected_resource_id.as_ref() == Some(&resource_id) {
            return;
        }

        self.load_builds_for_app(state, resource_id, window, cx);
    }

    pub fn render_section(
        &self,
        state: &ServicesPageState,
        window: &mut Window,
        cx: &mut Context<ServicesPage>,
    ) -> AnyElement {
        match state.navigation_id.as_str() {
            "builds" => self
                .render_builds_content(state, window, cx)
                .into_any_element(),
            "release" => self
                .render_release_content(state, window, cx)
                .into_any_element(),
            _ => self
                .render_overview_content(state, window, cx)
                .into_any_element(),
        }
    }

    fn navigation_workflows(&self, state: &ServicesPageState) -> Vec<&ServiceWorkflowDescriptor> {
        self.descriptor
            .workflows
            .iter()
            .filter(|workflow| {
                state.navigation_id == "release" && workflow.kind == ServiceWorkflowKind::Release
            })
            .collect()
    }

    fn available_targets<'a>(
        &'a self,
        workflows: &[&'a ServiceWorkflowDescriptor],
    ) -> Vec<ServiceWorkflowOption> {
        let supported_target_ids = workflows
            .iter()
            .flat_map(|workflow| workflow.target_ids.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>();

        self.descriptor
            .targets
            .iter()
            .filter(|target| supported_target_ids.contains(&target.id))
            .map(|target| ServiceWorkflowOption {
                id: target.id.clone(),
                label: target.label.clone(),
                detail: target.detail.clone(),
            })
            .collect()
    }

    fn available_workflows(&self, state: &ServicesPageState) -> Vec<ServiceWorkflowOption> {
        self.navigation_workflows(state)
            .into_iter()
            .filter(|workflow| workflow.supports_target(state.selected_target_id.as_deref()))
            .map(|workflow| ServiceWorkflowOption {
                id: workflow.id.clone(),
                label: workflow.label.clone(),
                detail: Some(workflow.detail.clone()),
            })
            .collect()
    }

    fn selected_workflow_descriptor(
        &self,
        state: &ServicesPageState,
    ) -> Option<&ServiceWorkflowDescriptor> {
        let selected_workflow_id = state.selected_workflow_id.as_deref()?;
        self.navigation_workflows(state)
            .into_iter()
            .find(|workflow| workflow.id == selected_workflow_id)
    }

    fn selected_workflow_form(
        &self,
        state: &ServicesPageState,
    ) -> Option<&ServiceWorkflowFormState> {
        let workflow_id = state.selected_workflow_id.as_ref()?;
        self.workflow_form_by_id(workflow_id)
    }

    fn selected_workflow_form_mut(
        &mut self,
        state: &ServicesPageState,
    ) -> Option<&mut ServiceWorkflowFormState> {
        let workflow_id = state.selected_workflow_id.as_ref()?;
        self.workflow_form_by_id_mut(workflow_id)
    }

    fn workflow_form_by_id(&self, workflow_id: &str) -> Option<&ServiceWorkflowFormState> {
        self.workflow_forms.get(workflow_id)
    }

    fn workflow_form_by_id_mut(
        &mut self,
        workflow_id: &str,
    ) -> Option<&mut ServiceWorkflowFormState> {
        self.workflow_forms.get_mut(workflow_id)
    }

    fn normalize_workflow_state(&self, state: &mut ServicesPageState) {
        let workflows = self.navigation_workflows(state);
        if workflows.is_empty() {
            state.selected_target_id = None;
            state.selected_workflow_id = None;
            return;
        }

        let available_targets = self.available_targets(&workflows);
        if available_targets.is_empty() {
            state.selected_target_id = None;
        } else if !available_targets
            .iter()
            .any(|target| Some(target.id.as_str()) == state.selected_target_id.as_deref())
        {
            state.selected_target_id = available_targets.first().map(|target| target.id.clone());
        }

        let available_workflows = self.available_workflows(state);
        if !available_workflows
            .iter()
            .any(|workflow| Some(workflow.id.as_str()) == state.selected_workflow_id.as_deref())
        {
            state.selected_workflow_id = available_workflows
                .first()
                .map(|workflow| workflow.id.clone());
        }
    }

    fn load_builds_for_app(
        &mut self,
        state: &mut ServicesPageState,
        app_id: String,
        window: &mut Window,
        cx: &mut Context<ServicesPage>,
    ) {
        let Some(app) = self.apps().iter().find(|app| app.id == app_id).cloned() else {
            self.builds_state = LoadState::Ready(AscBuildListState::default());
            cx.notify();
            return;
        };

        state.selected_resource_id = Some(app.id.clone());
        self.builds_state = LoadState::Loading;
        self.latest_run = None;
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let builds_result = cx
                .background_spawn(async move { load_builds_page(&app, None).await })
                .await;
            this.update_in(cx, |page, _window, cx| {
                with_app_store_connect_provider_mut(page, |pane, state| {
                    if state.selected_resource_id.as_deref() != Some(app_id.as_str()) {
                        return;
                    }

                    pane.builds_state = match builds_result {
                        Ok(page) => LoadState::Ready(AscBuildListState {
                            builds: page.builds,
                            next_page_url: page.next_page_url,
                            is_loading_more: false,
                            load_more_error: None,
                        }),
                        Err(error) => LoadState::Error(error.to_string()),
                    };
                    cx.notify();
                });
            })
            .ok();
        })
        .detach();
    }

    fn refresh_builds(
        &mut self,
        state: &mut ServicesPageState,
        window: &mut Window,
        cx: &mut Context<ServicesPage>,
    ) {
        if let Some(app_id) = state.selected_resource_id.clone() {
            self.load_builds_for_app(state, app_id, window, cx);
        }
    }

    fn load_more_builds(
        &mut self,
        state: &ServicesPageState,
        window: &mut Window,
        cx: &mut Context<ServicesPage>,
    ) {
        let Some(app_id) = state.selected_resource_id.clone() else {
            return;
        };
        let Some(app) = self.apps().iter().find(|app| app.id == app_id).cloned() else {
            return;
        };

        let next_page_url = match &mut self.builds_state {
            LoadState::Ready(builds_state) => {
                if builds_state.is_loading_more {
                    return;
                }

                let Some(next_page_url) = builds_state.next_page_url.clone() else {
                    return;
                };
                builds_state.is_loading_more = true;
                builds_state.load_more_error = None;
                next_page_url
            }
            LoadState::Loading | LoadState::Error(_) => return,
        };

        cx.notify();

        let request_next_page_url = next_page_url.clone();
        cx.spawn_in(window, async move |this, cx| {
            let builds_result = cx
                .background_spawn(async move {
                    load_builds_page(&app, Some(request_next_page_url)).await
                })
                .await;
            this.update_in(cx, |page, _window, cx| {
                with_app_store_connect_provider_mut(page, |pane, state| {
                    if state.selected_resource_id.as_deref() != Some(app_id.as_str()) {
                        return;
                    }

                    let LoadState::Ready(builds_state) = &mut pane.builds_state else {
                        return;
                    };
                    if builds_state.next_page_url.as_deref() != Some(next_page_url.as_str()) {
                        return;
                    }

                    builds_state.is_loading_more = false;
                    match builds_result {
                        Ok(page) => {
                            builds_state.builds.extend(page.builds);
                            builds_state.next_page_url = page.next_page_url;
                            builds_state.load_more_error = None;
                        }
                        Err(error) => {
                            builds_state.load_more_error = Some(error.to_string());
                        }
                    }
                    cx.notify();
                });
            })
            .ok();
        })
        .detach();
    }

    fn show_authenticate_form(&mut self) {
        self.auth_form.show();
    }

    fn cancel_authenticate_form(&mut self) {
        self.auth_form.cancel();
    }

    fn submit_authenticate(&mut self, window: &mut Window, cx: &mut Context<ServicesPage>) {
        let request = match self
            .auth_form
            .build_authenticate_request(APP_STORE_CONNECT_PROVIDER_ID, cx)
        {
            Ok(request) => request,
            Err(error) => {
                self.auth_form.set_error(error);
                cx.notify();
                return;
            }
        };

        self.auth_form.set_pending(true);
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { run_auth_action(request).await })
                .await;
            this.update_in(cx, |page, window, cx| {
                with_app_store_connect_provider_mut(page, |pane, state| match result {
                    Ok(()) => {
                        pane.auth_form.finish_success();
                        pane.refresh(state, window, cx);
                    }
                    Err(error) => {
                        pane.auth_form.set_pending(false);
                        pane.auth_form.set_error(error.to_string());
                        cx.notify();
                    }
                });
            })
            .ok();
        })
        .detach();
    }

    fn logout(&mut self, window: &mut Window, cx: &mut Context<ServicesPage>) {
        let Some(request) = self
            .auth_form
            .build_logout_request(APP_STORE_CONNECT_PROVIDER_ID)
        else {
            return;
        };

        self.auth_form.set_pending(true);
        self.auth_form.error_message = None;
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { run_auth_action(request).await })
                .await;
            this.update_in(cx, |page, window, cx| {
                with_app_store_connect_provider_mut(page, |pane, state| match result {
                    Ok(()) => {
                        pane.auth_form.finish_success();
                        pane.refresh(state, window, cx);
                    }
                    Err(error) => {
                        pane.auth_form.set_pending(false);
                        pane.auth_form.set_error(error.to_string());
                        cx.notify();
                    }
                });
            })
            .ok();
        })
        .detach();
    }

    fn pick_auth_file(
        &mut self,
        field_key: String,
        window: &mut Window,
        cx: &mut Context<ServicesPage>,
    ) {
        let workspace = {
            let page = cx.entity().read(cx);
            page.workspace().clone()
        };

        let prompt = workspace
            .update(cx, |workspace, cx| {
                workspace.prompt_for_open_path(
                    gpui::PathPromptOptions {
                        files: true,
                        directories: false,
                        multiple: false,
                        prompt: Some(SharedString::from("Select an App Store Connect key file")),
                    },
                    DirectoryLister::Local(
                        workspace.project().clone(),
                        workspace.app_state().fs.clone(),
                    ),
                    window,
                    cx,
                )
            })
            .ok();

        let Some(prompt) = prompt else {
            return;
        };

        cx.spawn_in(window, async move |this, cx| {
            let path = match prompt.await {
                Ok(Some(mut paths)) => paths.pop(),
                Ok(None) => None,
                Err(error) => {
                    this.update(cx, |page, cx| {
                        with_app_store_connect_provider_mut(page, |pane, _state| {
                            pane.auth_form.set_error(error.to_string());
                            cx.notify();
                        });
                    })
                    .ok();
                    None
                }
            };

            let Some(path) = path else {
                return;
            };

            this.update_in(cx, |page, window, cx| {
                with_app_store_connect_provider_mut(page, |pane, _state| {
                    pane.auth_form
                        .set_text(&field_key, &path.to_string_lossy(), window, cx);
                    cx.notify();
                });
            })
            .ok();
        })
        .detach();
    }

    fn pick_workflow_file(
        &mut self,
        state: &ServicesPageState,
        field_key: String,
        window: &mut Window,
        cx: &mut Context<ServicesPage>,
    ) {
        let Some(workflow_id) = state.selected_workflow_id.clone() else {
            return;
        };
        let Some(form) = self.workflow_form_by_id_mut(&workflow_id) else {
            return;
        };
        form.clear_error();

        let workspace = {
            let page = cx.entity().read(cx);
            page.workspace().clone()
        };

        let prompt = workspace
            .update(cx, |workspace, cx| {
                workspace.prompt_for_open_path(
                    gpui::PathPromptOptions {
                        files: true,
                        directories: false,
                        multiple: false,
                        prompt: Some(SharedString::from("Select an App Store Connect artifact")),
                    },
                    DirectoryLister::Local(
                        workspace.project().clone(),
                        workspace.app_state().fs.clone(),
                    ),
                    window,
                    cx,
                )
            })
            .ok();

        let Some(prompt) = prompt else {
            return;
        };

        cx.spawn_in(window, async move |this, cx| {
            let path = match prompt.await {
                Ok(Some(mut paths)) => paths.pop(),
                Ok(None) => None,
                Err(error) => {
                    this.update(cx, |page, cx| {
                        with_app_store_connect_provider_mut(page, |pane, _state| {
                            if let Some(form) = pane.workflow_form_by_id_mut(&workflow_id) {
                                form.set_error(error.to_string());
                            }
                            cx.notify();
                        });
                    })
                    .ok();
                    None
                }
            };

            let Some(path) = path else {
                return;
            };

            this.update_in(cx, |page, window, cx| {
                with_app_store_connect_provider_mut(page, |pane, _state| {
                    if let Some(form) = pane.workflow_form_by_id(&workflow_id) {
                        form.set_text(&field_key, &path.to_string_lossy(), window, cx);
                    }
                    cx.notify();
                });
            })
            .ok();
        })
        .detach();
    }

    fn select_target(&mut self, state: &mut ServicesPageState, target_id: String) {
        if state.selected_target_id.as_ref() == Some(&target_id) {
            return;
        }

        state.selected_target_id = Some(target_id);
        self.normalize_workflow_state(state);
        self.latest_run = None;
    }

    fn select_workflow(&mut self, state: &mut ServicesPageState, workflow_id: String) {
        if state.selected_workflow_id.as_ref() == Some(&workflow_id) {
            return;
        }

        state.selected_workflow_id = Some(workflow_id);
        self.latest_run = None;
    }

    fn workflow_ui_model(&self, state: &ServicesPageState) -> Option<ServiceWorkflowUiModel> {
        let workflows = self.available_workflows(state);
        if workflows.is_empty() {
            return None;
        }

        let form = self.selected_workflow_form(state)?.clone();
        let descriptor = self.selected_workflow_descriptor(state)?;
        let selected_app = self.selected_app(state);
        let disabled_reason = if selected_app.is_none() && descriptor.resource_kind.is_some() {
            Some("Select an app first.".into())
        } else {
            None
        };

        Some(ServiceWorkflowUiModel {
            provider_id: self.descriptor.id.clone(),
            target_label: "Target".into(),
            selected_target_id: state.selected_target_id.clone(),
            targets: self.available_targets(&self.navigation_workflows(state)),
            workflow_label: "Workflow".into(),
            selected_workflow_id: state.selected_workflow_id.clone(),
            workflows,
            execute_label: match descriptor.kind {
                ServiceWorkflowKind::Deploy => format!("Run {}", descriptor.label).into(),
                ServiceWorkflowKind::Release => format!("Run {}", descriptor.label).into(),
                ServiceWorkflowKind::Status => format!("Run {}", descriptor.label).into(),
            },
            form,
            run: self
                .latest_run
                .as_ref()
                .filter(|run| {
                    Some(run.workflow.as_str()) == state.selected_workflow_id.as_deref()
                        && run.target_id == state.selected_target_id
                })
                .map(|run| ServiceWorkflowRunSummary {
                    state: run.state.clone(),
                    headline: run.headline.clone(),
                    detail: run.detail.clone(),
                }),
            disabled_reason,
        })
    }

    fn submit_workflow(
        &mut self,
        state: &mut ServicesPageState,
        window: &mut Window,
        cx: &mut Context<ServicesPage>,
    ) {
        let Some(selected_app) = self.selected_app(state) else {
            return;
        };
        let Some(descriptor) = self.selected_workflow_descriptor(state).cloned() else {
            return;
        };
        let workflow_id = descriptor.id.clone();
        let target_id = state.selected_target_id.clone();
        let Some(form) = self.selected_workflow_form_mut(state) else {
            return;
        };

        let input = match form.build_input(cx) {
            Ok(input) => input,
            Err(error) => {
                form.set_error(error);
                cx.notify();
                return;
            }
        };
        form.set_pending(true);
        form.clear_error();

        let request = ServiceWorkflowRequest {
            provider_id: APP_STORE_CONNECT_PROVIDER_ID.to_string(),
            workflow: workflow_id.clone(),
            target_id: target_id.clone(),
            resource: Some(ServiceResourceRef {
                provider_id: APP_STORE_CONNECT_PROVIDER_ID.to_string(),
                kind: "app".to_string(),
                external_id: selected_app.id.clone(),
                label: selected_app.name.clone(),
            }),
            artifact: None,
            input,
        };
        self.latest_run = Some(ServiceRunDescriptor {
            workflow: workflow_id.clone(),
            target_id: target_id.clone(),
            state: ServiceRunState::Running,
            headline: descriptor.label.clone(),
            detail: format!("Running {} for {}.", descriptor.label, selected_app.name),
            output: None,
        });
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let workflow_label = descriptor.label.clone();
            let run_result = {
                let result = cx
                    .background_spawn(async move { run_workflow(request).await })
                    .await;
                match result {
                    Ok(execution) => WorkflowExecutionResult {
                        output: execution.combined_output(),
                        error: None,
                    },
                    Err(error) => WorkflowExecutionResult {
                        output: String::new(),
                        error: Some(error.to_string()),
                    },
                }
            };

            this.update_in(cx, |page, _window, cx| {
                with_app_store_connect_provider_mut(page, |pane, _state| {
                    if let Some(form) = pane.workflow_form_by_id_mut(&workflow_id) {
                        match &run_result.error {
                            Some(error) => form.set_error(error.clone()),
                            None => form.finish_success(),
                        }
                    }

                    match run_result.error {
                        Some(error) => {
                            pane.latest_run = Some(ServiceRunDescriptor {
                                workflow: workflow_id.clone(),
                                target_id: target_id.clone(),
                                state: ServiceRunState::Failed,
                                headline: format!("{workflow_label} failed"),
                                detail: error,
                                output: None,
                            });
                        }
                        None => {
                            let detail = summarize_workflow_output(&run_result.output);
                            pane.latest_run = Some(ServiceRunDescriptor {
                                workflow: workflow_id.clone(),
                                target_id: target_id.clone(),
                                state: ServiceRunState::Succeeded,
                                headline: format!("{workflow_label} finished"),
                                detail,
                                output: None,
                            });
                        }
                    }

                    cx.notify();
                });
            })
            .ok();
        })
        .detach();
    }

    fn apps(&self) -> &[AscAppSummary] {
        match &self.apps_state {
            LoadState::Ready(apps) => apps,
            LoadState::Loading | LoadState::Error(_) => &[],
        }
    }

    fn selected_app(&self, state: &ServicesPageState) -> Option<AscAppSummary> {
        state
            .selected_resource_id
            .as_ref()
            .and_then(|selected_id| self.apps().iter().find(|app| &app.id == selected_id))
            .cloned()
    }

    fn selected_build(&self) -> Option<&AscBuildSummary> {
        match &self.builds_state {
            LoadState::Ready(builds_state) => builds_state.builds.first(),
            LoadState::Loading | LoadState::Error(_) => None,
        }
    }

    fn auth_status_summary(&self) -> ServiceAuthStatusSummary {
        match &self.auth_state {
            LoadState::Loading => ServiceAuthStatusSummary {
                severity: Severity::Success,
                headline: "Checking authentication…".to_string(),
                detail: "Validating the current App Store Connect profile.".to_string(),
                warnings: Vec::new(),
                authenticated: false,
            },
            LoadState::Error(error) => ServiceAuthStatusSummary {
                severity: Severity::Warning,
                headline: "Authentication check failed".to_string(),
                detail: error.clone(),
                warnings: Vec::new(),
                authenticated: false,
            },
            LoadState::Ready(summary) => ServiceAuthStatusSummary {
                severity: if summary.healthy {
                    Severity::Success
                } else {
                    Severity::Warning
                },
                headline: summary.headline.clone(),
                detail: summary.detail.clone(),
                warnings: summary.warnings.clone(),
                authenticated: summary.authenticated,
            },
        }
    }

    fn render_detail_row(
        &self,
        title: impl Into<SharedString>,
        value: impl Into<SharedString>,
    ) -> impl IntoElement {
        h_flex()
            .justify_between()
            .gap_3()
            .child(Label::new(title).size(LabelSize::Small).color(Color::Muted))
            .child(
                Label::new(value)
                    .size(LabelSize::Small)
                    .single_line()
                    .truncate(),
            )
    }

    fn render_empty_panel(
        &self,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
        cx: &App,
    ) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_2()
            .p_5()
            .rounded_xl()
            .border_1()
            .border_color(cx.theme().colors().border_variant)
            .bg(cx.theme().colors().background)
            .child(Label::new(title))
            .child(
                Label::new(detail)
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
    }

    fn render_build_status(&self, build: &AscBuildSummary) -> impl IntoElement {
        v_flex()
            .gap_0p5()
            .child(render_state_line(
                &build.processing_state,
                format_processing_state,
            ))
            .when_some(build.testflight_external_state.as_ref(), |cell, state| {
                cell.child(render_state_line(
                    &format!("TestFlight {state}"),
                    format_embedded_state,
                ))
            })
            .when_some(build.app_store_state.as_ref(), |cell, state| {
                cell.child(render_state_line(
                    &format!("App Store {state}"),
                    format_embedded_state,
                ))
            })
    }

    fn render_builds_table_header(&self, cx: &App) -> impl IntoElement {
        h_flex()
            .w_full()
            .items_center()
            .gap_4()
            .px_3()
            .py_2()
            .bg(cx.theme().colors().background)
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .child(
                div().min_w(rems(8.)).w(rems(8.)).child(
                    Label::new("Platform")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
            )
            .child(
                div().min_w(rems(10.)).w(rems(10.)).child(
                    Label::new("Release Type")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
            )
            .child(
                div().min_w(rems(12.)).w(rems(12.)).child(
                    Label::new("Date")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
            )
            .child(
                div().min_w(rems(12.)).w(rems(12.)).child(
                    Label::new("Status")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
            )
            .child(
                div().min_w(rems(12.)).w(rems(12.)).child(
                    Label::new("Build")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
            )
            .child(
                div().min_w(rems(14.)).flex_grow().child(
                    Label::new("Version")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
            )
    }

    fn render_builds_table_row(
        &self,
        build: &AscBuildSummary,
        row_index: usize,
        cx: &App,
    ) -> impl IntoElement {
        let row_background = if row_index.is_multiple_of(2) {
            cx.theme().colors().editor_background
        } else {
            cx.theme().colors().background
        };

        h_flex()
            .w_full()
            .flex_none()
            .items_start()
            .gap_4()
            .px_3()
            .py_3()
            .bg(row_background)
            .when(row_index > 0, |row| {
                row.border_t_1()
                    .border_color(cx.theme().colors().border_variant)
            })
            .child(div().min_w(rems(8.)).w(rems(8.)).child(
                Label::new(format_platform(build.platform.as_deref())).size(LabelSize::Small),
            ))
            .child(
                div()
                    .min_w(rems(10.))
                    .w(rems(10.))
                    .child(Label::new(build_release_type(build)).size(LabelSize::Small)),
            )
            .child(
                v_flex()
                    .min_w(rems(12.))
                    .w(rems(12.))
                    .gap_0p5()
                    .child(
                        Label::new(format_build_date(&build.uploaded_date)).size(LabelSize::Small),
                    )
                    .when_some(build.expiration_date.as_ref(), |cell, expiration_date| {
                        cell.child(
                            Label::new(format!("Expires {}", format_build_date(expiration_date)))
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                    }),
            )
            .child(
                div()
                    .min_w(rems(16.))
                    .w(rems(16.))
                    .child(self.render_build_status(build)),
            )
            .child(
                div()
                    .min_w(rems(12.))
                    .w(rems(12.))
                    .child(Label::new(build.build_number.clone()).size(LabelSize::Small)),
            )
            .child(
                v_flex().min_w(rems(14.)).flex_grow().child(
                    Label::new(
                        build
                            .marketing_version
                            .clone()
                            .unwrap_or_else(|| "Unknown Version".to_string()),
                    )
                    .size(LabelSize::Small),
                ),
            )
    }

    fn render_builds_table(
        &self,
        builds: &[AscBuildSummary],
        cx: &App,
    ) -> impl IntoElement + use<> {
        v_flex()
            .w_full()
            .min_w_0()
            .flex_none()
            .gap_0()
            .rounded_xl()
            .border_1()
            .border_color(cx.theme().colors().border_variant)
            .overflow_hidden()
            .child(self.render_builds_table_header(cx))
            .children(
                builds
                    .iter()
                    .enumerate()
                    .map(|(row_index, build)| self.render_builds_table_row(build, row_index, cx)),
            )
    }

    fn render_release_content(
        &self,
        state: &ServicesPageState,
        _window: &mut Window,
        cx: &mut Context<ServicesPage>,
    ) -> impl IntoElement {
        let run = self.latest_run.as_ref().filter(|run| {
            matches!(
                run.workflow.as_str(),
                "publish_testflight" | "publish_appstore"
            ) && run.target_id == state.selected_target_id
        });

        v_flex()
            .size_full()
            .min_h_0()
            .gap_4()
            .when_some(run, |this, run| {
                this.child(
                    v_flex()
                        .gap_2()
                        .p_5()
                        .rounded_xl()
                        .border_1()
                        .border_color(cx.theme().colors().border_variant)
                        .bg(cx.theme().colors().background)
                        .child(Label::new(run.headline.clone()).size(LabelSize::Large))
                        .child(
                            Label::new(run.detail.clone())
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                )
            })
            .when(self.selected_app(state).is_none(), |this| {
                this.child(self.render_empty_panel(
                    "No app selected",
                    "Choose an app to publish a build from the shared workflow controls above.",
                    cx,
                ))
            })
    }

    fn render_overview_content(
        &self,
        state: &ServicesPageState,
        _window: &mut Window,
        cx: &mut Context<ServicesPage>,
    ) -> impl IntoElement {
        let selected_app = self.selected_app(state);
        let selected_build = self.selected_build();

        v_flex()
            .size_full()
            .min_h_0()
            .gap_4()
            .when_some(selected_app.clone(), |this, app| {
                this.child(
                    h_flex()
                        .gap_3()
                        .flex_wrap()
                        .child(
                            v_flex()
                                .h(rems(18.))
                                .min_w(rems(24.))
                                .flex_1()
                                .gap_4()
                                .justify_between()
                                .p_5()
                                .rounded_xl()
                                .border_1()
                                .border_color(cx.theme().colors().border_variant)
                                .bg(cx.theme().colors().background)
                                .child(
                                    v_flex()
                                        .gap_1()
                                        .child(Label::new("App Details").size(LabelSize::Small))
                                        .child(
                                            Label::new(app.name.clone())
                                                .size(LabelSize::Large)
                                                .single_line()
                                                .truncate(),
                                        )
                                        .child(
                                            Label::new(app.bundle_id.clone())
                                                .size(LabelSize::Small)
                                                .color(Color::Muted)
                                                .single_line()
                                                .truncate(),
                                        ),
                                )
                                .child(
                                    v_flex()
                                        .gap_3()
                                        .child(self.render_detail_row("SKU", app.sku.clone()))
                                        .child(
                                            self.render_detail_row(
                                                "Primary Locale",
                                                app.primary_locale
                                                    .clone()
                                                    .unwrap_or_else(|| "Not Set".to_string()),
                                            ),
                                        )
                                        .child(self.render_detail_row("App ID", app.id.clone())),
                                ),
                        )
                        .child(
                            v_flex()
                                .h(rems(18.))
                                .min_w(rems(24.))
                                .flex_1()
                                .gap_4()
                                .justify_between()
                                .p_5()
                                .rounded_xl()
                                .border_1()
                                .border_color(cx.theme().colors().border_variant)
                                .bg(cx.theme().colors().background)
                                .child(Label::new("Latest Build").size(LabelSize::Small))
                                .when_some(selected_build, |panel, build| {
                                    panel
                                        .child(
                                            v_flex()
                                                .gap_1()
                                                .child(
                                                    Label::new(
                                                        build
                                                            .marketing_version
                                                            .clone()
                                                            .unwrap_or_else(|| {
                                                                "Unknown Version".to_string()
                                                            }),
                                                    )
                                                    .size(LabelSize::Large),
                                                )
                                                .child(
                                                    Label::new(format!(
                                                        "Build {}",
                                                        build.build_number
                                                    ))
                                                    .size(LabelSize::Small)
                                                    .color(Color::Muted),
                                                ),
                                        )
                                        .child(
                                            v_flex()
                                                .gap_3()
                                                .child(self.render_detail_row(
                                                    "Platform",
                                                    format_platform(build.platform.as_deref()),
                                                ))
                                                .child(self.render_detail_row(
                                                    "Status",
                                                    build_status_summary(build),
                                                ))
                                                .child(self.render_detail_row(
                                                    "Uploaded",
                                                    format_build_date(&build.uploaded_date),
                                                ))
                                                .child(
                                                    self.render_detail_row(
                                                        "Expires",
                                                        build
                                                            .expiration_date
                                                            .as_ref()
                                                            .map(|date| format_build_date(date))
                                                            .unwrap_or_else(|| {
                                                                "Not Set".to_string()
                                                            }),
                                                    ),
                                                ),
                                        )
                                })
                                .when(selected_build.is_none(), |panel| {
                                    panel.child(
                                        Label::new("No builds are available for the selected app.")
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                                }),
                        ),
                )
            })
            .when(selected_app.is_none(), |this| {
                this.child(self.render_empty_panel(
                    "No app selected",
                    "Choose an app from the top bar to inspect its release data.",
                    cx,
                ))
            })
    }

    fn render_builds_content(
        &self,
        state: &ServicesPageState,
        window: &mut Window,
        cx: &mut Context<ServicesPage>,
    ) -> impl IntoElement {
        let selected_app = self.selected_app(state);
        let content = match &self.builds_state {
            LoadState::Loading => Label::new("Loading builds…")
                .color(Color::Muted)
                .into_any_element(),
            LoadState::Error(error) => v_flex()
                .gap_1()
                .child(Label::new("Could not load builds").color(Color::Error))
                .child(
                    Label::new(error.clone())
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element(),
            LoadState::Ready(_) if selected_app.is_none() => {
                Label::new("Select an app to load its builds.")
                    .color(Color::Muted)
                    .into_any_element()
            }
            LoadState::Ready(builds_state) if builds_state.builds.is_empty() => {
                Label::new("No builds were returned for the selected app.")
                    .color(Color::Muted)
                    .into_any_element()
            }
            LoadState::Ready(builds_state) if selected_app.is_some() => {
                let is_loading_more = builds_state.is_loading_more;
                let load_more_error = builds_state.load_more_error.clone();
                let has_next_page = builds_state.next_page_url.is_some();
                v_flex()
                    .gap_4()
                    .child(self.render_builds_table(&builds_state.builds, cx))
                    .when(has_next_page || load_more_error.is_some(), |this| {
                        this.child(
                            v_flex()
                                .w_full()
                                .items_center()
                                .gap_2()
                                .when_some(load_more_error, |this, error| {
                                    this.child(
                                        Label::new(error)
                                            .size(LabelSize::Small)
                                            .color(Color::Error),
                                    )
                                })
                                .when(has_next_page, |this| {
                                    this.child(
                                        Button::new("services-load-more-builds", "Load more")
                                            .style(ButtonStyle::Subtle)
                                            .size(ButtonSize::Compact)
                                            .disabled(is_loading_more)
                                            .on_click(cx.listener(|page, _, window, cx| {
                                                with_app_store_connect_provider_mut(
                                                    page,
                                                    |pane, state| {
                                                        pane.load_more_builds(state, window, cx);
                                                    },
                                                );
                                            })),
                                    )
                                }),
                        )
                    })
                    .into_any_element()
            }
            LoadState::Ready(_) => Label::new("Select an app to load its builds.")
                .color(Color::Muted)
                .into_any_element(),
        };

        v_flex().size_full().min_h_0().child(
            v_flex()
                .flex_1()
                .min_h_0()
                .gap_3()
                .child(
                    h_flex().justify_end().child(
                        IconButton::new("services-refresh-builds", IconName::RotateCw)
                            .style(ButtonStyle::Subtle)
                            .size(ButtonSize::Compact)
                            .disabled(selected_app.is_none())
                            .on_click(cx.listener(|page, _, window, cx| {
                                with_app_store_connect_provider_mut(page, |pane, state| {
                                    pane.refresh_builds(state, window, cx);
                                });
                            })),
                    ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(
                            v_flex()
                                .id("services-builds-scroll-content")
                                .track_scroll(&self.builds_scroll_handle)
                                .size_full()
                                .min_w_0()
                                .overflow_y_scroll()
                                .child(content),
                        )
                        .vertical_scrollbar_for(&self.builds_scroll_handle, window, cx),
                ),
        )
    }
}

impl ServiceWorkspaceAdapter for AppStoreConnectWorkspaceProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn descriptor(&self) -> &ServiceProviderDescriptor {
        self.descriptor()
    }

    fn normalize_state(&self, state: &mut ServicesPageState) {
        self.normalize_state(state);
    }

    fn refresh(
        &mut self,
        state: &mut ServicesPageState,
        window: &mut Window,
        cx: &mut Context<ServicesPage>,
    ) {
        self.refresh(state, window, cx);
    }

    fn resource_menu(&self, state: &ServicesPageState) -> Option<ServiceResourceMenuModel> {
        self.resource_menu(state)
    }

    fn select_resource(
        &mut self,
        state: &mut ServicesPageState,
        resource_id: String,
        window: &mut Window,
        cx: &mut Context<ServicesPage>,
    ) {
        self.select_resource(state, resource_id, window, cx);
    }

    fn render_section(
        &self,
        state: &ServicesPageState,
        window: &mut Window,
        cx: &mut Context<ServicesPage>,
    ) -> AnyElement {
        self.render_section(state, window, cx)
    }

    fn workflow_ui_model(&self, state: &ServicesPageState) -> Option<ServiceWorkflowUiModel> {
        self.workflow_ui_model(state)
    }

    fn handle_workflow_ui_action(
        &mut self,
        state: &mut ServicesPageState,
        action: ServiceWorkflowUiAction,
        window: &mut Window,
        cx: &mut Context<ServicesPage>,
    ) {
        match action {
            ServiceWorkflowUiAction::SelectTarget { target_id } => {
                self.select_target(state, target_id);
                cx.notify();
            }
            ServiceWorkflowUiAction::SelectWorkflow { workflow_id } => {
                self.select_workflow(state, workflow_id);
                cx.notify();
            }
            ServiceWorkflowUiAction::Submit => self.submit_workflow(state, window, cx),
            ServiceWorkflowUiAction::PickFile { field_key } => {
                self.pick_workflow_file(state, field_key, window, cx);
            }
            ServiceWorkflowUiAction::SetToggle { field_key, value } => {
                if let Some(form) = self.selected_workflow_form_mut(state) {
                    form.set_toggle(&field_key, value);
                }
                cx.notify();
            }
        }
    }

    fn auth_ui_model(&self) -> Option<ServiceAuthUiModel> {
        Some(ServiceAuthUiModel {
            provider_id: self.descriptor.id.clone(),
            authenticate_label: "Authenticate".into(),
            reauthenticate_label: "Re-authenticate".into(),
            logout_label: "Log Out".into(),
            status: self.auth_status_summary(),
            form: self.auth_form.clone(),
        })
    }

    fn handle_auth_ui_action(
        &mut self,
        _state: &mut ServicesPageState,
        action: ServiceAuthUiAction,
        window: &mut Window,
        cx: &mut Context<ServicesPage>,
    ) {
        match action {
            ServiceAuthUiAction::ShowAuthenticate => {
                self.show_authenticate_form();
                cx.notify();
            }
            ServiceAuthUiAction::CancelAuthenticate => {
                self.cancel_authenticate_form();
                cx.notify();
            }
            ServiceAuthUiAction::SubmitAuthenticate => self.submit_authenticate(window, cx),
            ServiceAuthUiAction::Logout => self.logout(window, cx),
            ServiceAuthUiAction::PickFile { field_key } => {
                self.pick_auth_file(field_key, window, cx);
            }
            ServiceAuthUiAction::SetToggle { field_key, value } => {
                self.auth_form.set_toggle(&field_key, value);
                cx.notify();
            }
        }
    }
}

#[derive(Clone, Debug)]
struct WorkflowExecutionResult {
    output: String,
    error: Option<String>,
}

fn summarize_workflow_output(output: &str) -> String {
    output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
        .unwrap_or_else(|| "Workflow completed successfully.".to_string())
}

#[derive(Deserialize)]
struct AscAppsResponse {
    data: Vec<AscAppRecord>,
}

#[derive(Deserialize)]
struct AscAppRecord {
    id: String,
    attributes: AscAppAttributes,
}

#[derive(Deserialize)]
struct AscAppAttributes {
    name: String,
    #[serde(rename = "bundleId")]
    bundle_id: String,
    sku: String,
    #[serde(rename = "primaryLocale")]
    primary_locale: Option<String>,
}

#[derive(Deserialize)]
struct AscBuildsResponse {
    data: Vec<AscBuildRecord>,
    #[serde(default)]
    included: Vec<AscPreReleaseVersionRecord>,
    #[serde(default)]
    links: AscPaginationLinks,
}

#[derive(Deserialize)]
struct AscBuildRecord {
    id: String,
    attributes: AscBuildAttributes,
    #[serde(default)]
    relationships: AscBuildRelationships,
}

#[derive(Deserialize)]
struct AscBuildAttributes {
    version: String,
    #[serde(rename = "uploadedDate")]
    uploaded_date: String,
    #[serde(rename = "expirationDate")]
    expiration_date: Option<String>,
    #[serde(rename = "processingState")]
    processing_state: String,
}

#[derive(Default, Deserialize)]
struct AscBuildRelationships {
    #[serde(rename = "preReleaseVersion")]
    pre_release_version: Option<AscBuildRelationship>,
}

#[derive(Deserialize)]
struct AscBuildRelationship {
    #[serde(default)]
    data: Option<AscRelatedResourceIdentifier>,
}

#[derive(Deserialize)]
struct AscRelatedResourceIdentifier {
    id: String,
}

#[derive(Deserialize)]
struct AscPreReleaseVersionRecord {
    #[serde(rename = "type")]
    resource_type: String,
    id: String,
    attributes: AscPreReleaseVersionAttributes,
}

#[derive(Clone, Deserialize)]
struct AscPreReleaseVersionAttributes {
    version: String,
    platform: String,
}

#[derive(Default, Deserialize)]
struct AscPaginationLinks {
    #[serde(default)]
    next: String,
}

async fn load_apps() -> Result<Vec<AscAppSummary>> {
    let response: AscAppsResponse = run_json_operation(ServiceOperationRequest {
        provider_id: APP_STORE_CONNECT_PROVIDER_ID.to_string(),
        operation: "list_apps".to_string(),
        resource: None,
        artifact: None,
        input: [("paginate".to_string(), "true".to_string())]
            .into_iter()
            .collect(),
    })
    .await?;

    let mut apps = response
        .data
        .into_iter()
        .map(|app| AscAppSummary {
            id: app.id,
            name: app.attributes.name,
            bundle_id: app.attributes.bundle_id,
            sku: app.attributes.sku,
            primary_locale: app.attributes.primary_locale,
        })
        .collect::<Vec<_>>();
    apps.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.bundle_id.cmp(&right.bundle_id))
    });
    Ok(apps)
}

async fn load_builds_page(
    app: &AscAppSummary,
    next_page_url: Option<String>,
) -> Result<AscBuildPage> {
    let input = if let Some(next_page_url) = next_page_url {
        [("next".to_string(), next_page_url)].into_iter().collect()
    } else {
        [
            ("limit".to_string(), ASC_BUILDS_PAGE_SIZE.to_string()),
            ("sort".to_string(), "-uploadedDate".to_string()),
        ]
        .into_iter()
        .collect()
    };
    let response: AscBuildsResponse = run_json_operation(ServiceOperationRequest {
        provider_id: APP_STORE_CONNECT_PROVIDER_ID.to_string(),
        operation: "list_builds".to_string(),
        resource: Some(ServiceResourceRef {
            provider_id: APP_STORE_CONNECT_PROVIDER_ID.to_string(),
            kind: "app".to_string(),
            external_id: app.id.clone(),
            label: app.name.clone(),
        }),
        artifact: None,
        input,
    })
    .await?;

    Ok(build_page_from_response(response))
}

fn build_page_from_response(response: AscBuildsResponse) -> AscBuildPage {
    let pre_release_versions = response
        .included
        .into_iter()
        .filter(|record| record.resource_type == "preReleaseVersions")
        .map(|record| (record.id, record.attributes))
        .collect::<BTreeMap<_, _>>();

    let builds = response
        .data
        .into_iter()
        .map(|build| {
            let pre_release_version = build
                .relationships
                .pre_release_version
                .and_then(|relationship| relationship.data)
                .and_then(|identifier| pre_release_versions.get(&identifier.id).cloned());

            AscBuildSummary {
                id: build.id,
                build_number: build.attributes.version,
                marketing_version: pre_release_version
                    .as_ref()
                    .map(|version| version.version.clone()),
                platform: pre_release_version
                    .as_ref()
                    .map(|version| version.platform.clone()),
                processing_state: build.attributes.processing_state,
                uploaded_date: build.attributes.uploaded_date,
                expiration_date: build.attributes.expiration_date,
                testflight_internal_state: None,
                testflight_external_state: None,
                app_store_version_id: None,
                app_store_state: None,
            }
        })
        .collect::<Vec<_>>();

    AscBuildPage {
        builds,
        next_page_url: non_empty_string(response.links.next),
    }
}

fn format_platform(platform: Option<&str>) -> String {
    match platform {
        Some("IOS") => "iOS".to_string(),
        Some("MAC_OS") => "macOS".to_string(),
        Some("TV_OS") => "tvOS".to_string(),
        Some("VISION_OS") => "visionOS".to_string(),
        Some(platform) => platform.replace('_', " "),
        None => "Unknown".to_string(),
    }
}

fn format_processing_state(processing_state: &str) -> String {
    processing_state
        .split('_')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut characters = segment.chars();
            let Some(first) = characters.next() else {
                return String::new();
            };

            format!(
                "{}{}",
                first.to_ascii_uppercase(),
                characters.as_str().to_ascii_lowercase()
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_build_date(timestamp: &str) -> String {
    let Some((date, time_with_offset)) = timestamp.split_once('T') else {
        return timestamp.to_string();
    };

    let time = time_with_offset.chars().take(5).collect::<String>();
    format!("{date} {time}")
}

fn build_release_type(build: &AscBuildSummary) -> String {
    let in_testflight =
        build.testflight_internal_state.is_some() || build.testflight_external_state.is_some();
    let in_app_store = build.app_store_version_id.is_some();

    match (in_testflight, in_app_store) {
        (true, true) => "TestFlight + App Store".to_string(),
        (true, false) => "TestFlight".to_string(),
        (false, true) => "App Store".to_string(),
        (false, false) => "Unknown".to_string(),
    }
}

fn build_status_summary(build: &AscBuildSummary) -> String {
    let mut states = vec![format_processing_state(&build.processing_state)];
    if let Some(state) = &build.testflight_external_state {
        states.push(format!("TF {}", format_processing_state(state)));
    }
    if let Some(state) = &build.app_store_state {
        states.push(format!("AS {}", format_processing_state(state)));
    }
    states.join(" · ")
}

fn color_for_state(value: &str) -> Color {
    let value = value.to_ascii_uppercase();
    if value.contains("REJECTED")
        || value.contains("INVALID")
        || value.contains("FAILED")
        || value.contains("ERROR")
    {
        Color::Error
    } else if value.contains("WAITING")
        || value.contains("IN_REVIEW")
        || value.contains("FOR_REVIEW")
        || value.contains("PROCESSING")
        || value.contains("PENDING")
        || value.contains("PREPARE")
        || value.contains("SUBMITTED")
    {
        Color::Warning
    } else if value.contains("READY")
        || value.contains("VALID")
        || value.contains("ACTIVE")
        || value.contains("APPROVED")
        || value.contains("TESTING")
    {
        Color::Success
    } else {
        Color::Muted
    }
}

fn render_state_line(value: &str, formatter: impl Fn(&str) -> String) -> impl IntoElement {
    let color = color_for_state(value);

    h_flex()
        .items_center()
        .gap_1p5()
        .child(Indicator::dot().color(color))
        .child(
            Label::new(formatter(value))
                .size(LabelSize::Small)
                .color(color),
        )
}

fn format_embedded_state(value: &str) -> String {
    let Some((label, state)) = value.split_once(' ') else {
        return format_processing_state(value);
    };
    format!("{label} {}", format_processing_state(state))
}

fn non_empty_string(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{AscBuildsResponse, build_page_from_response};

    #[test]
    fn parses_build_pages_without_eager_follow_up_requests() {
        let response: AscBuildsResponse = serde_json::from_value(json!({
            "data": [
                {
                    "id": "build-1",
                    "attributes": {
                        "version": "42",
                        "uploadedDate": "2026-04-05T12:34:56Z",
                        "expirationDate": "2026-05-05T12:34:56Z",
                        "processingState": "VALID"
                    },
                    "relationships": {
                        "preReleaseVersion": {
                            "data": {
                                "id": "prv-1"
                            }
                        }
                    }
                }
            ],
            "included": [
                {
                    "type": "preReleaseVersions",
                    "id": "prv-1",
                    "attributes": {
                        "version": "1.2.3",
                        "platform": "IOS"
                    }
                }
            ],
            "links": {
                "next": "https://api.appstoreconnect.apple.com/v1/builds?cursor=AQ"
            }
        }))
        .unwrap();

        let page = build_page_from_response(response);

        assert_eq!(page.builds.len(), 1);
        assert_eq!(page.builds[0].marketing_version.as_deref(), Some("1.2.3"));
        assert_eq!(page.builds[0].platform.as_deref(), Some("IOS"));
        assert_eq!(
            page.next_page_url.as_deref(),
            Some("https://api.appstoreconnect.apple.com/v1/builds?cursor=AQ")
        );
        assert!(page.builds[0].testflight_external_state.is_none());
        assert!(page.builds[0].app_store_state.is_none());
    }
}
