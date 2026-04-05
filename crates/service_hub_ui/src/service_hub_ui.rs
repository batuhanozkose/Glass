mod app_store_connect_auth;
mod app_store_connect_provider;
mod command_runner;
mod service_auth;
mod service_workflow;
mod services_page;
mod services_provider;

use gpui::{App, actions};
use services_page::ServicesPage;
use workspace::Workspace;

actions!(
    service_hub,
    [
        /// Opens service management for the current workspace.
        OpenServices
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace,
         window: Option<&mut gpui::Window>,
         _cx: &mut gpui::Context<Workspace>| {
            let Some(_) = window else {
                return;
            };

            workspace.register_action(move |workspace, _: &OpenServices, window, cx| {
                ServicesPage::open(workspace, window, cx);
            });
        },
    )
    .detach();
}
