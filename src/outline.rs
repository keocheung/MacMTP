use std::cell::Cell;

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSEvent, NSOutlineView};
use objc2_foundation::{MainThreadMarker, NSIndexSet, NSRange};

define_class!(
    #[unsafe(super = NSOutlineView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = PreviewOutlineViewIvars]
    pub struct PreviewOutlineView;

    impl PreviewOutlineView {
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let row = self.row_for_event(event);
            if row >= 0 && !self.isRowSelected(row) {
                self.ivars().drag_selection_anchor.set(row);
                self.select_drag_range(row);
                return;
            }

            unsafe {
                let _: () = msg_send![super(self), mouseDown: event];
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            if self.ivars().drag_selection_anchor.get() >= 0 {
                let row = self.row_for_event(event);
                if row >= 0 {
                    self.select_drag_range(row);
                }
                return;
            }

            unsafe {
                let _: () = msg_send![super(self), mouseDragged: event];
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            if self.ivars().drag_selection_anchor.get() >= 0 {
                self.ivars().drag_selection_anchor.set(-1);
                return;
            }

            unsafe {
                let _: () = msg_send![super(self), mouseUp: event];
            }
        }

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

#[derive(Default)]
pub struct PreviewOutlineViewIvars {
    drag_selection_anchor: Cell<isize>,
}

impl PreviewOutlineView {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(PreviewOutlineViewIvars {
            drag_selection_anchor: Cell::new(-1),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn row_for_event(&self, event: &NSEvent) -> isize {
        let window_point = event.locationInWindow();
        let point = self.convertPoint_fromView(window_point, None);
        self.rowAtPoint(point)
    }

    fn select_drag_range(&self, row: isize) {
        let anchor = self.ivars().drag_selection_anchor.get();
        if anchor < 0 || row < 0 {
            return;
        }

        let start = anchor.min(row) as usize;
        let end = anchor.max(row) as usize;
        let indexes = NSIndexSet::indexSetWithIndexesInRange(NSRange::new(start, end - start + 1));
        self.selectRowIndexes_byExtendingSelection(&indexes, false);
    }
}
