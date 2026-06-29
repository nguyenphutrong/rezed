use std::rc::Rc;

use gpui::{
    Anchor, AnyView, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Pixels, Point,
    Subscription,
};
use ui::{
    FluentBuilder as _, IntoElement, PopoverMenu, PopoverMenuHandle, PopoverTrigger, prelude::*,
};

use crate::{Picker, PickerDelegate};

pub struct PickerPopoverMenu<T, TT, P>
where
    T: PopoverTrigger + ButtonCommon,
    TT: Fn(&mut Window, &mut App) -> AnyView + 'static,
    P: PickerDelegate,
{
    picker: Entity<Picker<P>>,
    trigger: T,
    tooltip: TT,
    handle: Option<PopoverMenuHandle<Picker<P>>>,
    anchor: Anchor,
    offset: Option<Point<Pixels>>,
    on_open: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    _subscriptions: Vec<Subscription>,
}

impl<T, TT, P> PickerPopoverMenu<T, TT, P>
where
    T: PopoverTrigger + ButtonCommon,
    TT: Fn(&mut Window, &mut App) -> AnyView + 'static,
    P: PickerDelegate,
{
    pub fn new(
        picker: Entity<Picker<P>>,
        trigger: T,
        tooltip: TT,
        anchor: Anchor,
        cx: &mut App,
    ) -> Self {
        picker.update(cx, |picker, _| picker.set_popover());
        Self {
            _subscriptions: vec![cx.subscribe(&picker, |picker, &DismissEvent, cx| {
                picker.update(cx, |_, cx| cx.emit(DismissEvent));
            })],
            picker,
            trigger,
            tooltip,
            handle: None,
            offset: Some(Point {
                x: px(0.0),
                y: px(-2.0),
            }),
            on_open: None,
            anchor,
        }
    }

    pub fn with_handle(mut self, handle: PopoverMenuHandle<Picker<P>>) -> Self {
        self.handle = Some(handle);
        self
    }

    pub fn offset(mut self, offset: Point<Pixels>) -> Self {
        self.offset = Some(offset);
        self
    }

    pub fn on_open(mut self, on_open: Rc<dyn Fn(&mut Window, &mut App)>) -> Self {
        self.on_open = Some(on_open);
        self
    }
}

impl<T, TT, P> EventEmitter<DismissEvent> for PickerPopoverMenu<T, TT, P>
where
    T: PopoverTrigger + ButtonCommon,
    TT: Fn(&mut Window, &mut App) -> AnyView + 'static,
    P: PickerDelegate,
{
}

impl<T, TT, P> Focusable for PickerPopoverMenu<T, TT, P>
where
    T: PopoverTrigger + ButtonCommon,
    TT: Fn(&mut Window, &mut App) -> AnyView + 'static,
    P: PickerDelegate,
{
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl<T, TT, P> RenderOnce for PickerPopoverMenu<T, TT, P>
where
    T: PopoverTrigger + ButtonCommon,
    TT: Fn(&mut Window, &mut App) -> AnyView + 'static,
    P: PickerDelegate,
{
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let picker = self.picker.clone();

        PopoverMenu::new("popover-menu")
            .menu(move |_window, _cx| Some(picker.clone()))
            .trigger_with_tooltip(self.trigger, self.tooltip)
            .anchor(self.anchor)
            .when_some(self.on_open, |menu, on_open| menu.on_open(on_open))
            .when_some(self.handle, |menu, handle| menu.with_handle(handle))
            .when_some(self.offset, |menu, offset| menu.offset(offset))
    }
}
