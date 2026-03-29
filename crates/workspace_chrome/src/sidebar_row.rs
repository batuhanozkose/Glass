use gpui::{AnyElement, App, ClickEvent, RenderOnce, Window, div, px, rems};
use ui::{Icon, prelude::*};

#[derive(IntoElement)]
pub struct SidebarRow {
    id: SharedString,
    label: SharedString,
    icon: Option<IconName>,
    start_slot: Option<AnyElement>,
    selected: bool,
    disabled: bool,
    end_slot: Option<AnyElement>,
    on_click: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
}

impl SidebarRow {
    pub fn new(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        icon: IconName,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: Some(icon),
            start_slot: None,
            selected: false,
            disabled: false,
            end_slot: None,
            on_click: None,
        }
    }

    pub fn start_slot<E: IntoElement>(mut self, start_slot: E) -> Self {
        self.start_slot = Some(start_slot.into_any_element());
        self.icon = None;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn end_slot<E: IntoElement>(mut self, end_slot: E) -> Self {
        self.end_slot = Some(end_slot.into_any_element());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for SidebarRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let selected_background = theme.colors().text.opacity(0.14);
        let hover_background = theme.colors().text.opacity(0.09);
        let icon_color = if self.selected {
            Color::Default
        } else {
            Color::Muted
        };
        let label_color = if self.selected {
            theme.colors().text
        } else {
            theme.colors().text_muted
        };

        div()
            .id(self.id)
            .relative()
            .flex()
            .items_center()
            .w_full()
            .h(px(28.))
            .px_2()
            .gap_1()
            .rounded(theme.component_radius().tab.unwrap_or(px(8.0)))
            .when(self.selected, |this| this.bg(selected_background))
            .when(!self.selected && !self.disabled, |this| {
                this.hover(move |style| style.bg(hover_background))
            })
            .when(self.disabled, |this| this.opacity(0.5))
            .when_some(
                self.start_slot.or_else(|| {
                    self.icon.map(|icon| {
                        Icon::new(icon)
                            .size(IconSize::Small)
                            .color(icon_color)
                            .into_any_element()
                    })
                }),
                |this, start_slot| this.child(start_slot),
            )
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(rems(0.75))
                    .text_color(label_color)
                    .child(self.label),
            )
            .when_some(self.end_slot, |this, end_slot| this.child(end_slot))
            .when_some(self.on_click, |this, on_click| {
                this.cursor_pointer().on_click(on_click)
            })
    }
}
