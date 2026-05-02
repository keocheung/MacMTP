use std::cell::OnceCell;

use objc2::rc::Retained;
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSBox, NSEvent, NSTrackingArea, NSTrackingAreaOptions, NSView};
use objc2_foundation::{MainThreadMarker, NSRect};

define_class!(
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = DeviceRowViewIvars]
    pub struct DeviceRowView;

    impl DeviceRowView {
        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            self.set_hovered(true);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.set_hovered(false);
        }
    }
);

#[derive(Default)]
pub struct DeviceRowViewIvars {
    hover_background: OnceCell<Retained<NSBox>>,
    tracking_area: OnceCell<Retained<NSTrackingArea>>,
}

impl DeviceRowView {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DeviceRowViewIvars::default());
        unsafe { msg_send![super(this), init] }
    }

    pub fn set_hover_background(&self, background: Retained<NSBox>) {
        background.setHidden(true);
        self.ivars().hover_background.set(background).ok();
    }

    pub fn install_hover_tracking(&self) {
        let options = NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::ActiveInActiveApp
            | NSTrackingAreaOptions::InVisibleRect;
        let tracking_area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::default(),
                options,
                Some(self.as_ref()),
                None,
            )
        };
        self.addTrackingArea(&tracking_area);
        self.ivars().tracking_area.set(tracking_area).ok();
    }

    fn set_hovered(&self, hovered: bool) {
        if let Some(background) = self.ivars().hover_background.get() {
            background.setHidden(!hovered);
        }
    }
}
