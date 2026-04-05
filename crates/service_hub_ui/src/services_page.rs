use std::collections::BTreeMap;

use gpui::{
    App, Context, Corner, CursorStyle, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, Render, SharedString, Stateful, Subscription, WeakEntity, Window, div,
    point, prelude::FluentBuilder as _, px,
};
use service_hub::{ServiceHub, ServiceProviderDescriptor};
use ui::{
    AnyElement, Button, ButtonLike, ButtonSize, ButtonStyle, Checkbox, Clickable, Color,
    ContextMenu, Icon, IconButton, IconButtonShape, IconName, IconSize, Indicator, Label,
    LabelSize, PopoverMenu, Severity, TintColor, Toggleable, Tooltip, prelude::*,
};
use workspace::item::{Item, ItemBufferKind, ItemEvent};
use workspace::{Workspace, WorkspaceSidebarSection};
use workspace_chrome::SidebarRow;

use crate::service_auth::{
    ServiceAuthFieldState, ServiceAuthStatusSummary, ServiceAuthUiAction, ServiceAuthUiModel,
};
use crate::service_workflow::{
    ServiceWorkflowFieldState, ServiceWorkflowOption, ServiceWorkflowRunSummary,
    ServiceWorkflowUiAction, ServiceWorkflowUiModel,
};
use crate::services_provider::{
    ServiceWorkspacePane, ServicesPageState, build_service_workspace_panes,
    collect_provider_descriptors, normalize_services_page_state,
};

pub struct ServicesPage {
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
    providers: Vec<ServiceProviderDescriptor>,
    panes: BTreeMap<String, ServiceWorkspacePane>,
    state: ServicesPageState,
}

impl ServicesPage {
    pub fn open(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
        #[cfg(target_os = "macos")]
        Self::install_sidebar_section_view(workspace, cx);

        if let Some(existing) = workspace.item_of_type::<Self>(cx) {
            workspace.activate_item(&existing, true, true, window, cx);
            #[cfg(target_os = "macos")]
            workspace.select_sidebar_section(WorkspaceSidebarSection::Services, window, cx);
            return;
        }

        let page = Self::new(workspace, None, window, cx);
        workspace.add_item_to_active_pane(Box::new(page), None, true, window, cx);
        #[cfg(target_os = "macos")]
        workspace.select_sidebar_section(WorkspaceSidebarSection::Services, window, cx);
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn install_sidebar_section_view(
        workspace: &mut Workspace,
        cx: &mut Context<Workspace>,
    ) {
        let workspace_handle = workspace.weak_handle();
        let sidebar_panel = cx.new(|cx| ServicesSidebarPanel::new(workspace_handle, cx));
        workspace.set_sidebar_section_view(
            WorkspaceSidebarSection::Services,
            Some(sidebar_panel.into()),
            cx,
        );
    }

    fn new(
        workspace: &mut Workspace,
        initial_state: Option<ServicesPageState>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let panes = build_service_workspace_panes(ServiceHub::default().providers(), window, cx);
        let providers = collect_provider_descriptors(&panes);
        let state = normalize_services_page_state(&providers, initial_state);
        let workspace_handle = workspace.weak_handle();

        let page = cx.new(|cx| Self {
            focus_handle: cx.focus_handle(),
            workspace: workspace_handle,
            providers,
            panes,
            state,
        });

        page.update(cx, |page, cx| {
            page.normalize_active_provider_state();
            page.refresh_provider(window, cx);
        });
        page
    }

    pub(crate) fn workspace(&self) -> &WeakEntity<Workspace> {
        &self.workspace
    }

    pub(crate) fn with_provider_mut<R>(
        &mut self,
        provider_id: &str,
        callback: impl FnOnce(
            &mut dyn crate::services_provider::ServiceWorkspaceAdapter,
            &mut ServicesPageState,
        ) -> R,
    ) -> Option<R> {
        let pane = self.panes.get_mut(provider_id)?;
        Some(callback(pane.as_mut(), &mut self.state))
    }

    fn provider(&self) -> &ServiceProviderDescriptor {
        self.providers
            .iter()
            .find(|provider| provider.id == self.state.provider_id)
            .expect("selected provider should stay valid")
    }

    fn active_pane(&self) -> &ServiceWorkspacePane {
        self.panes
            .get(&self.state.provider_id)
            .expect("selected provider pane should stay valid")
    }

    fn normalize_active_provider_state(&mut self) {
        let provider_id = self.state.provider_id.clone();
        self.with_provider_mut(&provider_id, |pane, state| {
            pane.normalize_state(state);
        });
    }

    fn refresh_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let provider_id = self.state.provider_id.clone();
        self.with_provider_mut(&provider_id, |pane, state| {
            pane.refresh(state, window, cx);
        });
    }

    fn select_provider(
        &mut self,
        provider_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.provider_id == provider_id {
            return;
        }

        self.state.provider_id = provider_id;
        self.state.navigation_id = self.provider().shell.default_navigation_item_id.clone();
        self.state.selected_resource_id = None;
        self.state.selected_target_id = None;
        self.state.selected_workflow_id = None;
        self.normalize_active_provider_state();
        cx.emit(ItemEvent::UpdateTab);
        self.refresh_provider(window, cx);
    }

    fn select_navigation(&mut self, navigation_id: String, cx: &mut Context<Self>) {
        if self.state.navigation_id == navigation_id {
            return;
        }

        self.state.navigation_id = navigation_id;
        self.normalize_active_provider_state();
        cx.notify();
    }

    fn select_resource(
        &mut self,
        resource_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let provider_id = self.state.provider_id.clone();
        self.with_provider_mut(&provider_id, |pane, state| {
            pane.select_resource(state, resource_id, window, cx);
        });
    }

    fn render_provider_menu(
        &self,
        page: WeakEntity<Self>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let menu = ContextMenu::build(window, cx, |mut menu, _, _| {
            for provider in &self.providers {
                let provider_id = provider.id.clone();
                let page = page.clone();
                menu = menu.entry(provider.label.clone(), None, move |window, cx| {
                    page.update(cx, |this, cx| {
                        this.select_provider(provider_id.clone(), window, cx);
                    })
                    .ok();
                });
            }

            menu
        });

        Self::render_sidebar_popover_menu(
            "services-provider-menu",
            self.provider().label.clone(),
            menu,
        )
    }

    fn render_resource_menu(
        &self,
        page: WeakEntity<Self>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let resource_menu = self.active_pane().resource_menu(&self.state)?;
        let menu = ContextMenu::build(window, cx, |mut menu, _, _| {
            for entry in &resource_menu.entries {
                let resource_id = entry.id.clone();
                let label = match &entry.detail {
                    Some(detail) => format!("{} ({detail})", entry.label),
                    None => entry.label.clone(),
                };
                let page = page.clone();
                menu = menu.entry(label, None, move |window, cx| {
                    page.update(cx, |this, cx| {
                        this.select_resource(resource_id.clone(), window, cx);
                    })
                    .ok();
                });
            }

            menu
        });

        Some(
            v_flex()
                .gap_1()
                .child(
                    Label::new(resource_menu.singular_label.clone())
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .child(Self::render_sidebar_popover_menu(
                    "services-resource-menu",
                    resource_menu.current_label,
                    menu,
                ))
                .into_any_element(),
        )
    }

    fn render_sidebar_popover_menu(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        menu: Entity<ContextMenu>,
    ) -> impl IntoElement {
        let id = id.into();
        let label = label.into();
        PopoverMenu::new(format!("{id}-popover"))
            .full_width(true)
            .window_overlay()
            .menu(move |_window, _cx| Some(menu.clone()))
            .trigger(ServiceSidebarMenuTrigger::new(id, label))
            .attach(Corner::BottomLeft)
            .anchor(Corner::TopLeft)
            .offset(point(px(0.), px(4.)))
    }

    fn render_auth_sidebar_footer(
        &self,
        page: WeakEntity<Self>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let auth_ui = self.active_pane().auth_ui_model()?;
        let authenticate_label = if auth_ui.status.authenticated {
            auth_ui.reauthenticate_label.clone()
        } else {
            auth_ui.authenticate_label.clone()
        };
        let provider_id = auth_ui.provider_id.clone();
        let (indicator_color, status_tooltip) = Self::render_auth_status_indicator(&auth_ui.status);

        Some(
            v_flex()
                .gap_3()
                .pt_3()
                .border_t_1()
                .border_color(cx.theme().colors().border_variant)
                .child(
                    h_flex()
                        .justify_between()
                        .items_start()
                        .gap_2()
                        .child(
                            v_flex().min_w_0().gap_1().child(
                                h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        ButtonLike::new("services-auth-status-indicator")
                                            .style(ButtonStyle::Transparent)
                                            .size(ButtonSize::None)
                                            .cursor_style(CursorStyle::Arrow)
                                            .tooltip(Tooltip::text(status_tooltip))
                                            .child(Indicator::dot().color(indicator_color)),
                                    )
                                    .child(
                                        Label::new(auth_ui.status.detail.clone())
                                            .size(LabelSize::Small)
                                            .color(Color::Muted)
                                            .truncate(),
                                    ),
                            ),
                        )
                        .child(h_flex().items_center().gap_1().child(
                            if auth_ui.status.authenticated {
                                self.render_auth_overflow_menu(
                                    page.clone(),
                                    provider_id.clone(),
                                    window,
                                    cx,
                                    authenticate_label.clone(),
                                )
                                .into_any_element()
                            } else {
                                Button::new("services-auth-open", authenticate_label.clone())
                                    .style(ButtonStyle::Filled)
                                    .size(ButtonSize::Compact)
                                    .disabled(auth_ui.form.pending)
                                    .on_click({
                                        let page = page.clone();
                                        let provider_id = provider_id.clone();
                                        move |_, window, cx| {
                                            Self::dispatch_auth_action(
                                                &page,
                                                &provider_id,
                                                ServiceAuthUiAction::ShowAuthenticate,
                                                window,
                                                cx,
                                            );
                                        }
                                    })
                                    .into_any_element()
                            },
                        )),
                )
                .when_some(auth_ui.form.error_message.clone(), |this, error| {
                    this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
                })
                .when(
                    auth_ui.form.logout_available && auth_ui.status.authenticated,
                    |this| {
                        this.child(
                            SidebarRow::new(
                                "services-auth-logout-row",
                                auth_ui.logout_label.clone(),
                                IconName::Exit,
                            )
                            .on_click({
                                let page = page.clone();
                                let provider_id = provider_id.clone();
                                move |_, window, cx| {
                                    Self::dispatch_auth_action(
                                        &page,
                                        &provider_id,
                                        ServiceAuthUiAction::Logout,
                                        window,
                                        cx,
                                    );
                                }
                            }),
                        )
                    },
                )
                .when(auth_ui.form.expanded, |this| {
                    this.child(self.render_auth_form(
                        page.clone(),
                        auth_ui.clone(),
                        authenticate_label.clone(),
                        cx,
                    ))
                })
                .into_any_element(),
        )
    }

    fn render_auth_overflow_menu(
        &self,
        page: WeakEntity<Self>,
        provider_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
        authenticate_label: SharedString,
    ) -> impl IntoElement {
        let menu = ContextMenu::build(window, cx, |menu, _, _| {
            menu.entry(authenticate_label.clone(), None, {
                let page = page.clone();
                let provider_id = provider_id.clone();
                move |window, cx| {
                    Self::dispatch_auth_action(
                        &page,
                        &provider_id,
                        ServiceAuthUiAction::ShowAuthenticate,
                        window,
                        cx,
                    );
                }
            })
        });

        PopoverMenu::new("services-auth-overflow-menu")
            .window_overlay()
            .menu(move |_window, _cx| Some(menu.clone()))
            .trigger(
                IconButton::new("services-auth-overflow", IconName::Ellipsis)
                    .selected_style(ButtonStyle::Tinted(TintColor::Accent))
                    .shape(IconButtonShape::Square)
                    .style(ButtonStyle::Transparent)
                    .size(ButtonSize::Compact)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Authentication actions")),
            )
            .attach(Corner::BottomRight)
            .anchor(Corner::TopRight)
            .offset(point(px(0.), px(4.)))
    }

    fn render_auth_form(
        &self,
        page: WeakEntity<Self>,
        auth_ui: ServiceAuthUiModel,
        authenticate_label: SharedString,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().colors().border_variant)
            .bg(cx.theme().colors().background)
            .child(
                v_flex()
                    .gap_2()
                    .children(auth_ui.form.fields.iter().map(|field| {
                        match field {
                            ServiceAuthFieldState::Text { descriptor, input } => {
                                match descriptor.kind {
                                    service_hub::ServiceInputKind::FilePath => {
                                        h_flex()
                                            .items_end()
                                            .gap_2()
                                            .child(input.clone())
                                            .child(
                                                Button::new(
                                                    SharedString::from(format!(
                                                        "browse-auth-{}",
                                                        descriptor.key
                                                    )),
                                                    "Browse…",
                                                )
                                                .style(ButtonStyle::Outlined)
                                                .size(ButtonSize::Compact)
                                                .disabled(auth_ui.form.pending)
                                                .on_click({
                                                    let page = page.clone();
                                                    let provider_id = auth_ui.provider_id.clone();
                                                    let field_key = descriptor.key.clone();
                                                    move |_, window, cx| {
                                                        Self::dispatch_auth_action(
                                                            &page,
                                                            &provider_id,
                                                            ServiceAuthUiAction::PickFile {
                                                                field_key: field_key.clone(),
                                                            },
                                                            window,
                                                            cx,
                                                        );
                                                    }
                                                }),
                                            )
                                            .into_any_element()
                                    }
                                    service_hub::ServiceInputKind::Text
                                    | service_hub::ServiceInputKind::Toggle => {
                                        input.clone().into_any_element()
                                    }
                                }
                            }
                            ServiceAuthFieldState::Toggle { descriptor, value } => Checkbox::new(
                                SharedString::from(format!("auth-toggle-{}", descriptor.key)),
                                *value,
                            )
                            .label(descriptor.label.clone())
                            .disabled(auth_ui.form.pending)
                            .on_click(cx.listener({
                                let page = page.clone();
                                let provider_id = auth_ui.provider_id.clone();
                                let field_key = descriptor.key.clone();
                                move |_page, checked, window, cx| {
                                    Self::dispatch_auth_action(
                                        &page,
                                        &provider_id,
                                        ServiceAuthUiAction::SetToggle {
                                            field_key: field_key.clone(),
                                            value: *checked,
                                        },
                                        window,
                                        cx,
                                    );
                                }
                            }))
                            .into_any_element(),
                        }
                    }))
                    .child(
                        h_flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("services-auth-cancel", "Cancel")
                                    .style(ButtonStyle::Outlined)
                                    .size(ButtonSize::Compact)
                                    .disabled(auth_ui.form.pending)
                                    .on_click({
                                        let page = page.clone();
                                        let provider_id = auth_ui.provider_id.clone();
                                        move |_, window, cx| {
                                            Self::dispatch_auth_action(
                                                &page,
                                                &provider_id,
                                                ServiceAuthUiAction::CancelAuthenticate,
                                                window,
                                                cx,
                                            );
                                        }
                                    }),
                            )
                            .child(
                                Button::new("services-auth-submit", authenticate_label)
                                    .style(ButtonStyle::Filled)
                                    .size(ButtonSize::Compact)
                                    .disabled(auth_ui.form.pending)
                                    .on_click({
                                        let page = page.clone();
                                        let provider_id = auth_ui.provider_id.clone();
                                        move |_, window, cx| {
                                            Self::dispatch_auth_action(
                                                &page,
                                                &provider_id,
                                                ServiceAuthUiAction::SubmitAuthenticate,
                                                window,
                                                cx,
                                            );
                                        }
                                    }),
                            ),
                    ),
            )
    }

    fn render_auth_status_indicator(status: &ServiceAuthStatusSummary) -> (Color, String) {
        let indicator_color = match status.severity {
            Severity::Success => Color::Success,
            Severity::Warning => Color::Warning,
            Severity::Error => Color::Error,
            Severity::Info => Color::Info,
        };

        let mut tooltip_lines = vec![status.headline.clone(), status.detail.clone()];
        tooltip_lines.extend(status.warnings.clone());
        (indicator_color, tooltip_lines.join("\n"))
    }

    fn dispatch_auth_action(
        page: &WeakEntity<Self>,
        provider_id: &str,
        action: ServiceAuthUiAction,
        window: &mut Window,
        cx: &mut App,
    ) {
        page.update(cx, |page, cx| {
            page.with_provider_mut(provider_id, |pane, state| {
                pane.handle_auth_ui_action(state, action, window, cx);
            });
        })
        .ok();
    }

    pub(crate) fn render_sidebar_controls(
        page: &Entity<Self>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let page_handle = page.downgrade();
        page.update(cx, |page, cx| {
            page.render_sidebar_controls_inner(page_handle, window, cx)
                .into_any_element()
        })
    }

    fn render_sidebar_controls_inner(
        &self,
        page: WeakEntity<Self>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let controls = v_flex()
            .flex_1()
            .min_h_0()
            .gap_1()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .gap_2()
                            .child(
                                Label::new("Provider")
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(
                                IconButton::new("services-refresh", IconName::RotateCw)
                                    .shape(IconButtonShape::Square)
                                    .style(ButtonStyle::Transparent)
                                    .size(ButtonSize::Compact)
                                    .icon_size(IconSize::Small)
                                    .tooltip(Tooltip::text("Refresh"))
                                    .on_click({
                                        let page = page.clone();
                                        move |_, window, cx| {
                                            page.update(cx, |this, cx| {
                                                this.refresh_provider(window, cx);
                                            })
                                            .ok();
                                        }
                                    }),
                            ),
                    )
                    .child(self.render_provider_menu(page.clone(), window, cx)),
            )
            .when_some(
                self.render_resource_menu(page.clone(), window, cx),
                |this, resource_menu| this.child(resource_menu),
            )
            .child(
                v_flex()
                    .gap_1()
                    .children(self.provider().shell.navigation_items.iter().map(
                        |navigation_item| {
                            SidebarRow::new(
                                format!("services-nav-{}", navigation_item.id),
                                navigation_item.label.clone(),
                                Self::navigation_icon(&navigation_item.id),
                            )
                            .selected(self.state.navigation_id == navigation_item.id)
                            .on_click({
                                let navigation_id = navigation_item.id.clone();
                                let page = page.clone();
                                move |_, _window, cx| {
                                    page.update(cx, |this, cx| {
                                        this.select_navigation(navigation_id.clone(), cx);
                                    })
                                    .ok();
                                }
                            })
                        },
                    )),
            );

        v_flex()
            .size_full()
            .p_3()
            .gap_3()
            .child(controls)
            .when_some(
                self.render_auth_sidebar_footer(page.clone(), window, cx),
                |this, footer| this.child(footer),
            )
            .when_some(
                self.active_pane()
                    .render_sidebar_footer_extra(&self.state, window, cx),
                |this, footer| this.child(footer),
            )
    }

    fn render_provider_content(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let workflow_ui = self.active_pane().workflow_ui_model(&self.state);

        match workflow_ui {
            Some(workflow_ui) => v_flex()
                .size_full()
                .min_h_0()
                .gap_4()
                .child(self.render_workflow_surface(workflow_ui, window, cx))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(self.active_pane().render_section(&self.state, window, cx)),
                )
                .into_any_element(),
            None => self.active_pane().render_section(&self.state, window, cx),
        }
    }

    fn navigation_icon(navigation_id: &str) -> IconName {
        match navigation_id {
            "overview" => IconName::Info,
            "builds" => IconName::Box,
            "release" => IconName::ArrowCircle,
            _ => IconName::Globe,
        }
    }

    fn render_workflow_surface(
        &self,
        workflow_ui: ServiceWorkflowUiModel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let page = cx.entity().downgrade();
        let target_menu = if workflow_ui.targets.is_empty() {
            None
        } else {
            let menu = Self::build_workflow_option_menu(
                &workflow_ui.targets,
                page.clone(),
                workflow_ui.provider_id.clone(),
                window,
                cx,
                |option| ServiceWorkflowUiAction::SelectTarget {
                    target_id: option.id.clone(),
                },
            );
            Some(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(workflow_ui.target_label.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(Self::render_sidebar_popover_menu(
                        "services-workflow-target-menu",
                        workflow_ui
                            .selected_target_id
                            .as_ref()
                            .and_then(|selected_id| {
                                workflow_ui
                                    .targets
                                    .iter()
                                    .find(|target| &target.id == selected_id)
                                    .map(|target| target.label.clone())
                            })
                            .unwrap_or_else(|| workflow_ui.target_label.to_string()),
                        menu,
                    ))
                    .into_any_element(),
            )
        };

        let workflow_menu = if workflow_ui.workflows.is_empty() {
            None
        } else {
            let menu = Self::build_workflow_option_menu(
                &workflow_ui.workflows,
                page.clone(),
                workflow_ui.provider_id.clone(),
                window,
                cx,
                |option| ServiceWorkflowUiAction::SelectWorkflow {
                    workflow_id: option.id.clone(),
                },
            );
            Some(
                v_flex()
                    .gap_1()
                    .child(
                        Label::new(workflow_ui.workflow_label.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(Self::render_sidebar_popover_menu(
                        "services-workflow-menu",
                        workflow_ui
                            .selected_workflow_id
                            .as_ref()
                            .and_then(|selected_id| {
                                workflow_ui
                                    .workflows
                                    .iter()
                                    .find(|workflow| &workflow.id == selected_id)
                                    .map(|workflow| workflow.label.clone())
                            })
                            .unwrap_or_else(|| workflow_ui.workflow_label.to_string()),
                        menu,
                    ))
                    .into_any_element(),
            )
        };

        v_flex()
            .gap_3()
            .p_4()
            .rounded_xl()
            .border_1()
            .border_color(cx.theme().colors().border_variant)
            .bg(cx.theme().colors().background)
            .when_some(target_menu, |this, target_menu| this.child(target_menu))
            .when_some(workflow_menu, |this, workflow_menu| {
                this.child(workflow_menu)
            })
            .when(!workflow_ui.form.fields.is_empty(), |this| {
                this.child(self.render_workflow_form(page.clone(), workflow_ui.clone(), cx))
            })
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .gap_3()
                    .child(
                        v_flex()
                            .gap_1()
                            .when_some(workflow_ui.run.clone(), |this, run| {
                                let (color, headline) = Self::render_workflow_run(run);
                                this.child(Label::new(headline).size(LabelSize::Small).color(color))
                            })
                            .when_some(workflow_ui.disabled_reason.clone(), |this, reason| {
                                this.child(
                                    Label::new(reason)
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                            }),
                    )
                    .child(
                        Button::new(
                            "services-workflow-submit",
                            workflow_ui.execute_label.clone(),
                        )
                        .style(ButtonStyle::Filled)
                        .size(ButtonSize::Compact)
                        .disabled(
                            workflow_ui.form.pending
                                || workflow_ui.selected_workflow_id.is_none()
                                || workflow_ui.disabled_reason.is_some(),
                        )
                        .on_click({
                            let page = page.clone();
                            let provider_id = workflow_ui.provider_id.clone();
                            move |_, window, cx| {
                                Self::dispatch_workflow_action(
                                    &page,
                                    &provider_id,
                                    ServiceWorkflowUiAction::Submit,
                                    window,
                                    cx,
                                );
                            }
                        }),
                    ),
            )
            .when_some(workflow_ui.form.error_message.clone(), |this, error| {
                this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
            })
    }

    fn build_workflow_option_menu(
        options: &[ServiceWorkflowOption],
        page: WeakEntity<Self>,
        provider_id: String,
        window: &mut Window,
        cx: &mut App,
        build_action: impl Fn(&ServiceWorkflowOption) -> ServiceWorkflowUiAction + 'static + Copy,
    ) -> Entity<ContextMenu> {
        ContextMenu::build(window, cx, move |mut menu, _, _| {
            for option in options {
                let page = page.clone();
                let provider_id = provider_id.clone();
                let action = build_action(option);
                let label = match &option.detail {
                    Some(detail) => format!("{} ({detail})", option.label),
                    None => option.label.clone(),
                };
                menu = menu.entry(label, None, move |window, cx| {
                    Self::dispatch_workflow_action(&page, &provider_id, action.clone(), window, cx);
                });
            }

            menu
        })
    }

    fn render_workflow_form(
        &self,
        page: WeakEntity<Self>,
        workflow_ui: ServiceWorkflowUiModel,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .gap_2()
            .children(workflow_ui.form.fields.iter().map(|field| {
                match field {
                    ServiceWorkflowFieldState::Text { descriptor, input } => {
                        match descriptor.kind {
                            service_hub::ServiceInputKind::FilePath => h_flex()
                                .items_end()
                                .gap_2()
                                .child(input.clone())
                                .child(
                                    Button::new(
                                        SharedString::from(format!(
                                            "browse-workflow-{}",
                                            descriptor.key
                                        )),
                                        "Browse…",
                                    )
                                    .style(ButtonStyle::Outlined)
                                    .size(ButtonSize::Compact)
                                    .disabled(workflow_ui.form.pending)
                                    .on_click({
                                        let page = page.clone();
                                        let provider_id = workflow_ui.provider_id.clone();
                                        let field_key = descriptor.key.clone();
                                        move |_, window, cx| {
                                            Self::dispatch_workflow_action(
                                                &page,
                                                &provider_id,
                                                ServiceWorkflowUiAction::PickFile {
                                                    field_key: field_key.clone(),
                                                },
                                                window,
                                                cx,
                                            );
                                        }
                                    }),
                                )
                                .into_any_element(),
                            service_hub::ServiceInputKind::Text
                            | service_hub::ServiceInputKind::Toggle => {
                                input.clone().into_any_element()
                            }
                        }
                    }
                    ServiceWorkflowFieldState::Toggle { descriptor, value } => Checkbox::new(
                        SharedString::from(format!("workflow-toggle-{}", descriptor.key)),
                        *value,
                    )
                    .label(descriptor.label.clone())
                    .disabled(workflow_ui.form.pending)
                    .on_click(cx.listener({
                        let provider_id = workflow_ui.provider_id.clone();
                        let field_key = descriptor.key.clone();
                        move |page, checked, window, cx| {
                            page.with_provider_mut(&provider_id, |pane, state| {
                                pane.handle_workflow_ui_action(
                                    state,
                                    ServiceWorkflowUiAction::SetToggle {
                                        field_key: field_key.clone(),
                                        value: *checked,
                                    },
                                    window,
                                    cx,
                                );
                            });
                            cx.notify();
                        }
                    }))
                    .into_any_element(),
                }
            }))
    }

    fn render_workflow_run(run: ServiceWorkflowRunSummary) -> (Color, String) {
        let color = match run.state {
            service_hub::ServiceRunState::Pending | service_hub::ServiceRunState::Running => {
                Color::Muted
            }
            service_hub::ServiceRunState::Succeeded => Color::Success,
            service_hub::ServiceRunState::Failed => Color::Error,
        };

        let text = if run.detail.is_empty() {
            run.headline
        } else {
            format!("{}: {}", run.headline, run.detail)
        };
        (color, text)
    }

    fn dispatch_workflow_action(
        page: &WeakEntity<Self>,
        provider_id: &str,
        action: ServiceWorkflowUiAction,
        window: &mut Window,
        cx: &mut App,
    ) {
        page.update(cx, |page, cx| {
            page.with_provider_mut(provider_id, |pane, state| {
                pane.handle_workflow_ui_action(state, action, window, cx);
            });
        })
        .ok();
    }
}

#[derive(IntoElement)]
struct ServiceSidebarMenuTrigger {
    div: Stateful<gpui::Div>,
    label: SharedString,
    selected: bool,
}

impl ServiceSidebarMenuTrigger {
    fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            div: div().id(id.into()),
            label: label.into(),
            selected: false,
        }
    }
}

impl Clickable for ServiceSidebarMenuTrigger {
    fn on_click(
        mut self,
        handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.div = self.div.on_click(handler);
        self
    }

    fn cursor_style(mut self, cursor_style: CursorStyle) -> Self {
        self.div = self.div.cursor(cursor_style);
        self
    }
}

impl Toggleable for ServiceSidebarMenuTrigger {
    fn toggle_state(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl RenderOnce for ServiceSidebarMenuTrigger {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (text_color, background, hover_background, active_background) = match self.selected {
            false => (
                cx.theme().colors().text_muted,
                cx.theme().colors().tab_inactive_background.opacity(0.0),
                cx.theme().colors().text.opacity(0.09),
                cx.theme().colors().text.opacity(0.14),
            ),
            true => (
                cx.theme().colors().text,
                cx.theme().colors().text.opacity(0.14),
                cx.theme().colors().text.opacity(0.14),
                cx.theme().colors().text.opacity(0.20),
            ),
        };

        self.div
            .w_full()
            .h(px(28.))
            .bg(background)
            .rounded(cx.theme().component_radius().tab.unwrap_or(px(6.0)))
            .when(!self.selected, |this| {
                this.hover(move |style| style.bg(hover_background))
            })
            .active(move |style| style.bg(active_background))
            .cursor_pointer()
            .child(
                h_flex()
                    .w_full()
                    .h_full()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .gap_2()
                    .text_color(text_color)
                    .child(Label::new(self.label).size(LabelSize::Small).truncate())
                    .child(
                        Icon::new(IconName::ChevronUpDown)
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                    ),
            )
    }
}

impl EventEmitter<ItemEvent> for ServicesPage {}

impl Focusable for ServicesPage {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for ServicesPage {
    type Event = ItemEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        self.provider().label.clone().into()
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        Some("Services".into())
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Server))
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Services Page Opened")
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    fn buffer_kind(&self, _cx: &App) -> ItemBufferKind {
        ItemBufferKind::None
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        f(*event)
    }
}

impl Render for ServicesPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .p_5()
            .child(self.render_provider_content(window, cx))
    }
}

#[cfg(target_os = "macos")]
struct ServicesSidebarPanel {
    workspace: WeakEntity<Workspace>,
    observed_page: Option<WeakEntity<ServicesPage>>,
    subscriptions_initialized: bool,
    _workspace_subscription: Option<Subscription>,
    _page_subscription: Option<Subscription>,
}

#[cfg(target_os = "macos")]
impl ServicesSidebarPanel {
    fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        let mut panel = Self {
            workspace,
            observed_page: None,
            subscriptions_initialized: false,
            _workspace_subscription: None,
            _page_subscription: None,
        };

        if let Some(workspace) = panel.workspace.upgrade() {
            panel._workspace_subscription = Some(cx.observe(&workspace, |this, _, cx| {
                this.sync_page_subscription(cx);
                cx.notify();
            }));
        }

        panel
    }

    fn sync_page_subscription(&mut self, cx: &mut Context<Self>) {
        let next_page = self
            .workspace
            .upgrade()
            .and_then(|workspace| workspace.read(cx).item_of_type::<ServicesPage>(cx));
        let next_page_id = next_page.as_ref().map(|page| page.entity_id());
        let current_page_id = self
            .observed_page
            .as_ref()
            .and_then(|page| page.upgrade())
            .map(|page| page.entity_id());

        if next_page_id == current_page_id {
            return;
        }

        self._page_subscription = None;
        self.observed_page = next_page.as_ref().map(|page| page.downgrade());

        if let Some(page) = next_page {
            self._page_subscription = Some(cx.observe(&page, |this, _, cx| {
                this.sync_page_subscription(cx);
                cx.notify();
            }));
        }
    }
}

#[cfg(target_os = "macos")]
impl Render for ServicesSidebarPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.subscriptions_initialized {
            self.subscriptions_initialized = true;
            let this = cx.entity().downgrade();
            cx.defer(move |cx| {
                let Some(this) = this.upgrade() else {
                    return;
                };

                this.update(cx, |this, cx| {
                    this.sync_page_subscription(cx);
                    cx.notify();
                });
            });
        }

        if let Some(page) = self.observed_page.as_ref().and_then(|page| page.upgrade()) {
            return ServicesPage::render_sidebar_controls(&page, window, cx);
        }

        v_flex()
            .size_full()
            .justify_center()
            .p_4()
            .gap_2()
            .child(Label::new("Services").size(LabelSize::Large))
            .child(
                Label::new("Open the Service Hub to manage providers, apps, and releases.")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .into_any_element()
    }
}
