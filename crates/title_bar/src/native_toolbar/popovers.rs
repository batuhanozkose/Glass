use crate::TitleBar;
use gpui::{
    DismissEvent, NativePanel, NativePanelAnchor, NativePanelLevel, NativePanelStyle, Render,
    SharedString, Window,
};
use ui::ContextMenu;

impl TitleBar {
    fn dismiss_toolbar_overlay(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        self.native_toolbar_state.open_toolbar_overlay_item_id = None;
        window.dismiss_native_panel();
        cx.notify();
    }

    fn toggle_toolbar_hosted_overlay<V: Render>(
        &mut self,
        view: gpui::Entity<V>,
        item_id: SharedString,
        width: f64,
        height: f64,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self
            .native_toolbar_state
            .open_toolbar_overlay_item_id
            .as_ref()
            == Some(&item_id)
        {
            self.dismiss_toolbar_overlay(window, cx);
            return;
        }

        let weak_title_bar = cx.entity().downgrade();
        self.native_toolbar_state.open_toolbar_overlay_item_id = Some(item_id.clone());
        window.dismiss_native_panel();
        window.show_native_panel(
            NativePanel::new(width, height)
                .style(NativePanelStyle::Borderless)
                .level(NativePanelLevel::PopUpMenu)
                .transient(true)
                .corner_radius(12.0)
                .on_close(move |_, _, cx| {
                    weak_title_bar
                        .update(cx, |title_bar, cx| {
                            title_bar.native_toolbar_state.open_toolbar_overlay_item_id = None;
                            cx.notify();
                        })
                        .ok();
                })
                .content_view(view),
            NativePanelAnchor::ToolbarItem(item_id),
        );
        cx.notify();
    }

    fn show_toolbar_context_menu_overlay(
        &mut self,
        menu: gpui::Entity<ContextMenu>,
        item_id: &'static str,
        width: f64,
        height: f64,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let item_id: SharedString = item_id.into();
        if self
            .native_toolbar_state
            .open_toolbar_overlay_item_id
            .as_ref()
            == Some(&item_id)
        {
            self.dismiss_toolbar_overlay(window, cx);
            return;
        }

        let dismiss_title_bar = cx.entity().downgrade();
        window
            .subscribe(&menu, cx, move |_, _: &DismissEvent, window, cx| {
                dismiss_title_bar
                    .update(cx, |title_bar, cx| {
                        title_bar.dismiss_toolbar_overlay(window, cx);
                    })
                    .ok();
            })
            .detach();

        self.toggle_toolbar_hosted_overlay(menu, item_id, width, height, window, cx);
    }

    pub(crate) fn show_lsp_overlay(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(lsp_button) = self.right_item_view::<language_tools::lsp_button::LspButton>()
        else {
            return;
        };
        let menu = lsp_button.update(cx, |lsp_button, cx| {
            lsp_button.ensure_toolbar_menu(window, cx)
        });
        let Some(menu) = menu else {
            return;
        };

        self.show_toolbar_context_menu_overlay(
            menu,
            "glass.status.language_servers",
            320.0,
            420.0,
            window,
            cx,
        );
    }
}
