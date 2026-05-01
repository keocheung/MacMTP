use objc2::rc::Retained;
use objc2::{MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSEvent, NSOutlineView};
use objc2_foundation::MainThreadMarker;

define_class!(
    #[unsafe(super = NSOutlineView)]
    #[thread_kind = MainThreadOnly]
    pub struct PreviewOutlineView;

    impl PreviewOutlineView {
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if event.keyCode() == 49 {
                if let (Some(target), Some(action)) = (self.target(), self.doubleAction()) {
                    unsafe {
                        let _: bool = NSApplication::sharedApplication(self.mtm())
                            .sendAction_to_from(action, Some(&target), Some(self));
                    }
                    return;
                }
            }

            unsafe {
                let _: () = msg_send![super(self), keyDown: event];
            }
        }
    }
);

impl PreviewOutlineView {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}
