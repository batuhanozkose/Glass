use anyhow::Result;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, PathPromptOptions, Render,
    ScrollHandle, SharedString, WeakEntity,
};
use project::DirectoryLister;
use serde::Deserialize;
use service_hub::{ServiceHub, ServiceInputKind, ServiceOperationRequest, ServiceResourceRef};
use ui::{
    Banner, Button, ButtonStyle, Checkbox, Color, Label, LabelSize, ListItem, ListItemSpacing,
    Severity, WithScrollbar, prelude::*,
};
use workspace::Workspace;
use workspace::item::{Item, ItemBufferKind, ItemEvent};

use crate::app_store_connect_auth::{
    AscAuthSummary, ServiceAuthFieldState, ServiceAuthFormState, load_auth_status,
};
use crate::command_runner::{run_auth_action, run_json_operation};

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
    processing_state: String,
    uploaded_date: String,
    expiration_date: Option<String>,
    min_os_version: Option<String>,
}

#[derive(Clone, Debug)]
enum LoadState<T> {
    Loading,
    Ready(T),
    Error(String),
}

pub struct AppStoreConnectPage {
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
    provider: service_hub::ServiceProviderDescriptor,
    auth_form: ServiceAuthFormState,
    auth_state: LoadState<AscAuthSummary>,
    apps_state: LoadState<Vec<AscAppSummary>>,
    selected_app_id: Option<String>,
    builds_state: LoadState<Vec<AscBuildSummary>>,
    apps_scroll_handle: ScrollHandle,
    builds_scroll_handle: ScrollHandle,
}

impl AppStoreConnectPage {
    pub fn new(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let provider = ServiceHub::default()
            .providers()
            .into_iter()
            .find(|provider| provider.id == "app-store-connect")
            .expect("App Store Connect provider should be registered");
        let workspace_handle = workspace.weak_handle();

        let page = cx.new(|cx| Self {
            focus_handle: cx.focus_handle(),
            workspace: workspace_handle,
            auth_form: ServiceAuthFormState::new(&provider, window, cx),
            provider,
            auth_state: LoadState::Loading,
            apps_state: LoadState::Loading,
            selected_app_id: None,
            builds_state: LoadState::Ready(Vec::new()),
            apps_scroll_handle: ScrollHandle::new(),
            builds_scroll_handle: ScrollHandle::new(),
        });

        page.update(cx, |page, cx| page.refresh_overview(window, cx));
        page
    }

    fn refresh_overview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.auth_state = LoadState::Loading;
        self.apps_state = LoadState::Loading;
        self.builds_state = LoadState::Ready(Vec::new());
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
                .update_in(cx, |this, _window, cx| {
                    this.auth_state = match auth_result {
                        Ok(summary) => LoadState::Ready(summary),
                        Err(error) => LoadState::Error(error.to_string()),
                    };

                    match apps_result {
                        Ok(apps) => {
                            let next_selected_app_id = this
                                .selected_app_id
                                .as_ref()
                                .and_then(|selected_id| {
                                    apps.iter()
                                        .find(|app| &app.id == selected_id)
                                        .map(|app| app.id.clone())
                                })
                                .or_else(|| apps.first().map(|app| app.id.clone()));
                            this.apps_state = LoadState::Ready(apps);
                            this.selected_app_id = next_selected_app_id.clone();
                            this.builds_state = if next_selected_app_id.is_some() {
                                LoadState::Loading
                            } else {
                                LoadState::Ready(Vec::new())
                            };
                            cx.notify();
                            next_selected_app_id
                        }
                        Err(error) => {
                            this.apps_state = LoadState::Error(error.to_string());
                            this.selected_app_id = None;
                            this.builds_state = LoadState::Ready(Vec::new());
                            cx.notify();
                            None
                        }
                    }
                })
                .ok()
                .flatten();

            if let Some(app_id) = selected_app_id {
                this.update_in(cx, |this, window, cx| {
                    this.load_builds_for_app(app_id, window, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    fn load_builds_for_app(&mut self, app_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(app) = self.apps().iter().find(|app| app.id == app_id).cloned() else {
            self.builds_state = LoadState::Ready(Vec::new());
            cx.notify();
            return;
        };

        self.selected_app_id = Some(app.id.clone());
        self.builds_state = LoadState::Loading;
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let builds_result = cx
                .background_spawn(async move { load_builds(&app).await })
                .await;
            this.update_in(cx, |this, _window, cx| {
                this.builds_state = match builds_result {
                    Ok(builds) => LoadState::Ready(builds),
                    Err(error) => LoadState::Error(error.to_string()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn refresh_builds(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(app_id) = self.selected_app_id.clone() {
            self.load_builds_for_app(app_id, window, cx);
        }
    }

    fn select_app(&mut self, app_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_app_id.as_ref() == Some(&app_id) {
            return;
        }

        self.load_builds_for_app(app_id, window, cx);
    }

    fn show_authenticate_form(&mut self, cx: &mut Context<Self>) {
        self.auth_form.show();
        cx.notify();
    }

    fn cancel_authenticate_form(&mut self, cx: &mut Context<Self>) {
        self.auth_form.cancel();
        cx.notify();
    }

    fn submit_authenticate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let request = match self
            .auth_form
            .build_authenticate_request(&self.provider.id, cx)
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
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(()) => {
                        this.auth_form.finish_success();
                        this.refresh_overview(window, cx);
                    }
                    Err(error) => {
                        this.auth_form.set_pending(false);
                        this.auth_form.set_error(error.to_string());
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn logout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(request) = self.auth_form.build_logout_request(&self.provider.id) else {
            return;
        };

        self.auth_form.set_pending(true);
        self.auth_form.error_message = None;
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { run_auth_action(request).await })
                .await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(()) => {
                        this.auth_form.finish_success();
                        this.refresh_overview(window, cx);
                    }
                    Err(error) => {
                        this.auth_form.set_pending(false);
                        this.auth_form.set_error(error.to_string());
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn pick_auth_file(
        &mut self,
        field_key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prompt = self
            .workspace
            .update(cx, |workspace, cx| {
                workspace.prompt_for_open_path(
                    PathPromptOptions {
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
                    this.update(cx, |this, cx| {
                        this.auth_form.set_error(error.to_string());
                        cx.notify();
                    })
                    .ok();
                    None
                }
            };

            let Some(path) = path else {
                return;
            };

            this.update_in(cx, |this, window, cx| {
                this.auth_form
                    .set_text(&field_key, &path.to_string_lossy(), window, cx);
                cx.notify();
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

    fn selected_app(&self) -> Option<AscAppSummary> {
        self.selected_app_id
            .as_ref()
            .and_then(|selected_id| self.apps().iter().find(|app| &app.id == selected_id))
            .cloned()
    }

    fn render_auth_status_summary(&self) -> (Severity, String, String, Vec<String>, bool) {
        match &self.auth_state {
            LoadState::Loading => (
                Severity::Success,
                "Checking App Store Connect authentication…".to_string(),
                "Validating the configured App Store Connect profile.".to_string(),
                Vec::new(),
                false,
            ),
            LoadState::Error(error) => (
                Severity::Warning,
                "Authentication check failed".to_string(),
                error.clone(),
                Vec::new(),
                false,
            ),
            LoadState::Ready(summary) => (
                if summary.healthy {
                    Severity::Success
                } else {
                    Severity::Warning
                },
                summary.headline.clone(),
                summary.detail.clone(),
                summary.warnings.clone(),
                summary.authenticated,
            ),
        }
    }

    fn render_auth_banner(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (severity, headline, detail, warnings, authenticated) =
            self.render_auth_status_summary();
        let authenticate_label = if authenticated {
            "Re-authenticate"
        } else {
            "Authenticate"
        };

        Banner::new()
            .severity(severity)
            .child(
                v_flex()
                    .w_full()
                    .gap_3()
                    .child(
                        h_flex()
                            .justify_between()
                            .items_start()
                            .gap_3()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(Label::new(headline))
                                    .child(
                                        Label::new(detail)
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                                    .children(warnings.into_iter().map(|warning| {
                                        Label::new(warning)
                                            .size(LabelSize::Small)
                                            .color(Color::Muted)
                                    })),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new("asc-auth-open", authenticate_label)
                                            .style(if authenticated {
                                                ButtonStyle::Outlined
                                            } else {
                                                ButtonStyle::Filled
                                            })
                                            .disabled(self.auth_form.pending)
                                            .on_click(cx.listener(|this, _, _window, cx| {
                                                this.show_authenticate_form(cx);
                                            })),
                                    )
                                    .when(self.auth_form.logout_available && authenticated, |this| {
                                        this.child(
                                            Button::new("asc-auth-logout", "Log Out")
                                                .style(ButtonStyle::Outlined)
                                                .disabled(self.auth_form.pending)
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.logout(window, cx);
                                                })),
                                        )
                                    }),
                            ),
                    )
                    .when_some(self.auth_form.error_message.clone(), |this, error| {
                        this.child(
                            Label::new(error)
                                .size(LabelSize::Small)
                                .color(Color::Error),
                        )
                    })
                    .when(self.auth_form.expanded, |this| {
                        this.child(
                            v_flex()
                                .gap_2()
                                .children(self.auth_form.fields.iter().map(|field| match field {
                                    ServiceAuthFieldState::Text { descriptor, input } => {
                                        match descriptor.kind {
                                            ServiceInputKind::FilePath => h_flex()
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
                                                    .disabled(self.auth_form.pending)
                                                    .on_click(cx.listener({
                                                        let field_key = descriptor.key.clone();
                                                        move |this, _, window, cx| {
                                                            this.pick_auth_file(
                                                                field_key.clone(),
                                                                window,
                                                                cx,
                                                            );
                                                        }
                                                    })),
                                                )
                                                .into_any_element(),
                                            ServiceInputKind::Text | ServiceInputKind::Toggle => {
                                                input.clone().into_any_element()
                                            }
                                        }
                                    }
                                    ServiceAuthFieldState::Toggle { descriptor, value } => {
                                        Checkbox::new(
                                            SharedString::from(format!(
                                                "auth-toggle-{}",
                                                descriptor.key
                                            )),
                                            *value,
                                        )
                                        .label(descriptor.label.clone())
                                        .disabled(self.auth_form.pending)
                                        .on_click(cx.listener({
                                            let field_key = descriptor.key.clone();
                                            move |this, checked, _window, cx| {
                                                this.auth_form.set_toggle(&field_key, *checked);
                                                cx.notify();
                                            }
                                        }))
                                        .into_any_element()
                                    }
                                }))
                                .child(
                                    h_flex()
                                        .justify_end()
                                        .gap_2()
                                        .child(
                                            Button::new("asc-auth-cancel", "Cancel")
                                                .style(ButtonStyle::Outlined)
                                                .disabled(self.auth_form.pending)
                                                .on_click(cx.listener(
                                                    |this, _, _window, cx| {
                                                        this.cancel_authenticate_form(cx);
                                                    },
                                                )),
                                        )
                                        .child(
                                            Button::new("asc-auth-submit", authenticate_label)
                                                .style(ButtonStyle::Filled)
                                                .disabled(self.auth_form.pending)
                                                .on_click(cx.listener(
                                                    |this, _, window, cx| {
                                                        this.submit_authenticate(window, cx);
                                                    },
                                                )),
                                        ),
                                ),
                        )
                    }),
            )
    }

    fn render_apps_panel(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.apps_state {
            LoadState::Loading => Label::new("Loading apps…")
                .color(Color::Muted)
                .into_any_element(),
            LoadState::Error(error) => v_flex()
                .gap_1()
                .child(Label::new("Could not load apps").color(Color::Error))
                .child(
                    Label::new(error.clone())
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element(),
            LoadState::Ready(apps) if apps.is_empty() => Label::new(
                "No apps were returned by App Store Connect.",
            )
            .color(Color::Muted)
            .into_any_element(),
            LoadState::Ready(apps) => v_flex()
                .gap_1()
                .children(apps.iter().map(|app| {
                    let selected = self.selected_app_id.as_ref() == Some(&app.id);
                    ListItem::new(format!("asc-app-{}", app.id))
                        .inset(true)
                        .spacing(ListItemSpacing::Sparse)
                        .toggle_state(selected)
                        .child(
                            v_flex()
                                .gap_0p5()
                                .child(Label::new(app.name.clone()).single_line())
                                .child(
                                    Label::new(format!("{} · {}", app.bundle_id, app.id))
                                        .size(LabelSize::Small)
                                        .color(Color::Muted)
                                        .single_line(),
                                ),
                        )
                        .on_click(cx.listener({
                            let app_id = app.id.clone();
                            move |this, _, window, cx| {
                                this.select_app(app_id.clone(), window, cx);
                            }
                        }))
                }))
                .into_any_element(),
        };

        v_flex()
            .min_w(rems(22.))
            .w_96()
            .flex_1()
            .min_h_0()
            .gap_3()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().background)
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(Label::new("Apps"))
                    .child(
                        Button::new("asc-refresh-apps", "Refresh Apps")
                            .style(ButtonStyle::Outlined)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.refresh_overview(window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_flex()
                            .id("asc-apps-scroll-content")
                            .track_scroll(&self.apps_scroll_handle)
                            .size_full()
                            .min_w_0()
                            .overflow_y_scroll()
                            .child(content),
                    )
                    .vertical_scrollbar_for(&self.apps_scroll_handle, window, cx),
            )
    }

    fn render_builds_panel(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_app = self.selected_app();
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
            LoadState::Ready(builds) if builds.is_empty() => {
                Label::new("No builds were returned for the selected app.")
                    .color(Color::Muted)
                    .into_any_element()
            }
            LoadState::Ready(builds) => v_flex()
                .gap_1()
                .children(builds.iter().map(|build| {
                    let subtitle = match &build.expiration_date {
                        Some(expiration_date) => format!(
                            "{} · uploaded {} · expires {}",
                            build.processing_state, build.uploaded_date, expiration_date
                        ),
                        None => format!(
                            "{} · uploaded {}",
                            build.processing_state, build.uploaded_date
                        ),
                    };

                    ListItem::new(format!("asc-build-{}", build.id))
                        .inset(true)
                        .spacing(ListItemSpacing::Sparse)
                        .child(
                            v_flex()
                                .gap_0p5()
                                .child(
                                    Label::new(format!("Build {}", build.build_number))
                                        .single_line(),
                                )
                                .child(
                                    Label::new(subtitle)
                                        .size(LabelSize::Small)
                                        .color(Color::Muted)
                                        .single_line(),
                                ),
                        )
                }))
                .into_any_element(),
        };

        v_flex()
            .flex_1()
            .min_h_0()
            .gap_3()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().background)
            .child(
                h_flex()
                    .justify_between()
                    .items_start()
                    .gap_3()
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(Label::new("Builds"))
                            .child(
                                Label::new(match selected_app {
                                    Some(ref app) => format!("{} · {}", app.name, app.bundle_id),
                                    None => "Select an app to inspect its builds".to_string(),
                                })
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                            ),
                    )
                    .child(
                        Button::new("asc-refresh-builds", "Refresh Builds")
                            .style(ButtonStyle::Outlined)
                            .disabled(selected_app.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.refresh_builds(window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_flex()
                            .id("asc-builds-scroll-content")
                            .track_scroll(&self.builds_scroll_handle)
                            .size_full()
                            .min_w_0()
                            .overflow_y_scroll()
                            .child(content),
                    )
                    .vertical_scrollbar_for(&self.builds_scroll_handle, window, cx),
            )
    }
}

impl EventEmitter<ItemEvent> for AppStoreConnectPage {}

impl Focusable for AppStoreConnectPage {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for AppStoreConnectPage {
    type Event = ItemEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        "App Store Connect".into()
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("App Store Connect Page Opened")
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    fn buffer_kind(&self, _cx: &App) -> ItemBufferKind {
        ItemBufferKind::Singleton
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        f(*event)
    }
}

impl Render for AppStoreConnectPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .gap_4()
            .p_4()
            .bg(cx.theme().colors().editor_background)
            .child(self.render_auth_banner(cx))
            .child(
                h_flex()
                    .gap_4()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(self.render_apps_panel(window, cx))
                    .child(self.render_builds_panel(window, cx)),
            )
    }
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
}

#[derive(Deserialize)]
struct AscBuildRecord {
    id: String,
    attributes: AscBuildAttributes,
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
    #[serde(rename = "minOsVersion")]
    min_os_version: Option<String>,
}

async fn load_apps() -> Result<Vec<AscAppSummary>> {
    let response: AscAppsResponse = run_json_operation(ServiceOperationRequest {
        provider_id: "app-store-connect".to_string(),
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

async fn load_builds(app: &AscAppSummary) -> Result<Vec<AscBuildSummary>> {
    let response: AscBuildsResponse = run_json_operation(ServiceOperationRequest {
        provider_id: "app-store-connect".to_string(),
        operation: "list_builds".to_string(),
        resource: Some(ServiceResourceRef {
            provider_id: "app-store-connect".to_string(),
            kind: "app".to_string(),
            external_id: app.id.clone(),
            label: app.name.clone(),
        }),
        artifact: None,
        input: [
            ("paginate".to_string(), "true".to_string()),
            ("sort".to_string(), "-uploadedDate".to_string()),
        ]
        .into_iter()
        .collect(),
    })
    .await?;

    Ok(response
        .data
        .into_iter()
        .map(|build| AscBuildSummary {
            id: build.id,
            build_number: build.attributes.version,
            processing_state: build.attributes.processing_state,
            uploaded_date: build.attributes.uploaded_date,
            expiration_date: build.attributes.expiration_date,
            min_os_version: build.attributes.min_os_version,
        })
        .collect())
}
