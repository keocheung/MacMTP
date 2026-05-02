use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use block2::RcBlock;
use mtp_rs::mtp::{MtpDevice, MtpDeviceInfo};
use mtp_rs::{ObjectHandle, OperationCode, StorageId};
use objc2_quartz as _;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSAutoresizingMaskOptions,
    NSBackingStoreType, NSBox, NSBoxType, NSButton, NSColor, NSControlTextEditingDelegate,
    NSDragOperation, NSDraggingSession, NSEvent, NSFilePromiseProvider,
    NSFilePromiseProviderDelegate, NSFont, NSImage, NSImageView, NSLineBreakMode, NSOutlineView,
    NSOutlineViewDataSource, NSOutlineViewDelegate, NSPasteboard, NSPasteboardWriting,
    NSProgressIndicator, NSSplitView, NSSplitViewDelegate, NSTableColumn, NSTextAlignment,
    NSTextField, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSError, NSIndexSet, NSInteger, NSNotification, NSNumber, NSObject,
    NSObjectProtocol, NSOperationQueue, NSPoint, NSRect, NSSize, NSString, NSTimer, NSURL,
    ns_string,
};
use tokio::runtime::{Builder, Runtime};

use crate::device_row::DeviceRowView;
use crate::loc::{ns_tr, tr};
use crate::model::{BrowserNode, NodeSource, message_node};
use crate::mount::{self, MountHandle};
use crate::ui::{build_browser_ui, install_main_menu};
use crate::util::{format_bytes, format_mtp_datetime, format_mtp_error, sanitize_filename};

const DRAG_NODE_PREFIX: &str = "macmtp-node:";
const FILE_PROMISE_TYPE_FILE: &str = "public.data";
const FILE_PROMISE_TYPE_FOLDER: &str = "public.folder";
const FILE_PROMISE_ERROR_DOMAIN: &str = "MacMTPFilePromiseError";
const COPY_PROGRESS_THROTTLE: Duration = Duration::from_millis(120);
const LEFT_SIDEBAR_MIN_WIDTH: f64 = 180.0;
const RIGHT_SIDEBAR_MIN_WIDTH: f64 = 220.0;
const BROWSER_MIN_WIDTH: f64 = 260.0;

#[derive(Default)]
pub(crate) struct AppDelegateIvars {
    pub(crate) window: OnceCell<Retained<NSWindow>>,
    pub(crate) outline_view: OnceCell<Retained<NSOutlineView>>,
    pub(crate) device_list_view: OnceCell<Retained<NSView>>,
    pub(crate) refresh_button: OnceCell<Retained<NSButton>>,
    pub(crate) detail_mount_button: OnceCell<Retained<NSButton>>,
    pub(crate) detail_eject_button: OnceCell<Retained<NSButton>>,
    pub(crate) title_label: OnceCell<Retained<NSTextField>>,
    pub(crate) detail_label: OnceCell<Retained<NSTextField>>,
    pub(crate) detail_info_view: OnceCell<Retained<NSView>>,
    pub(crate) progress_indicator: OnceCell<Retained<NSProgressIndicator>>,
    runtime: OnceCell<Runtime>,
    devices: RefCell<Vec<MtpDeviceInfo>>,
    device: RefCell<Option<MtpDevice>>,
    current_device_location: RefCell<Option<u64>>,
    current_mount: RefCell<Option<MountHandle>>,
    current_mount_location: RefCell<Option<u64>>,
    current_mounting_location: RefCell<Option<u64>>,
    pending_mount_location: RefCell<Option<u64>>,
    current_mtp_lock: RefCell<Option<Arc<Mutex<()>>>>,
    device_row_views: RefCell<Vec<Retained<NSView>>>,
    detail_info_rows: RefCell<Vec<Retained<NSView>>>,
    nodes: RefCell<Vec<BrowserNode>>,
    root_children: RefCell<Vec<usize>>,
    mtp_locks: RefCell<HashMap<u64, Arc<Mutex<()>>>>,
    active_copies: Arc<AtomicUsize>,
    copy_events_tx: OnceCell<mpsc::Sender<CopyEvent>>,
    copy_events_rx: RefCell<Option<mpsc::Receiver<CopyEvent>>>,
    mount_events_tx: OnceCell<mpsc::Sender<MountEvent>>,
    mount_events_rx: RefCell<Option<mpsc::Receiver<MountEvent>>>,
    device_events_tx: OnceCell<mpsc::Sender<DeviceEvent>>,
    device_events_rx: RefCell<Option<mpsc::Receiver<DeviceEvent>>>,
    copy_error: RefCell<Option<String>>,
    copy_timer: OnceCell<Retained<NSTimer>>,
}

#[derive(Clone)]
struct ExportNode {
    name: String,
    storage_id: StorageId,
    handle: ObjectHandle,
    is_folder: bool,
}

enum CopyEvent {
    Started,
    Progress {
        name: String,
        bytes_done: u64,
        bytes_total: Option<u64>,
        files_done: usize,
    },
    Finished {
        result: Result<(), String>,
    },
}

enum MountEvent {
    Finished {
        location_id: u64,
        result: Result<MountHandle, String>,
    },
}

enum DeviceEvent {
    Connected {
        device_info: MtpDeviceInfo,
        result: Result<(MtpDevice, Vec<BrowserNode>, Vec<usize>), String>,
    },
}

struct SendCompletion(RcBlock<dyn Fn(*mut NSError)>);

unsafe impl Send for SendCompletion {}

impl SendCompletion {
    fn call_success(&self) {
        self.0.call((std::ptr::null_mut(),));
    }

    fn call_error(&self, message: &str) {
        let error = promise_error(message);
        self.0.call((Retained::autorelease_return(error),));
    }
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppDelegateIvars]
    pub(crate) struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, notification: &NSNotification) {
            let mtm = self.mtm();
            let app = notification
                .object()
                .unwrap()
                .downcast::<NSApplication>()
                .unwrap();

            self.ivars()
                .runtime
                .set(
                    Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("create tokio runtime"),
                )
                .ok();

            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(900.0, 560.0)),
                    NSWindowStyleMask::Titled
                        | NSWindowStyleMask::Closable
                        | NSWindowStyleMask::Miniaturizable
                        | NSWindowStyleMask::Resizable,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            unsafe { window.setReleasedWhenClosed(false) };
            window.setTitle(ns_string!("MacMTP"));
            window.setContentMinSize(NSSize::new(720.0, 420.0));
            window.setDelegate(Some(ProtocolObject::from_ref(self)));

            let content = window.contentView().expect("window must have a content view");
            build_browser_ui(self, mtm, &content);
            install_main_menu(&app, self, mtm);
            self.install_copy_event_timer();
            self.show_initial_device_prompt();
            self.refresh_devices();

            window.center();
            window.makeKeyAndOrderFront(None);
            self.ivars().window.set(window).unwrap();

            self.update_detail();

            app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            self.close_current_device();
        }
    }

    unsafe impl NSWindowDelegate for Delegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            self.close_current_device();
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
    }

    unsafe impl NSOutlineViewDataSource for Delegate {}
    unsafe impl NSOutlineViewDelegate for Delegate {}
    unsafe impl NSSplitViewDelegate for Delegate {
        #[unsafe(method(splitView:shouldAdjustSizeOfSubview:))]
        fn split_view_should_adjust_size_of_subview(
            &self,
            split_view: &NSSplitView,
            view: &NSView,
        ) -> bool {
            let subviews = split_view.subviews();
            if subviews.len() < 3 {
                return true.into();
            }
            let browser = unsafe { subviews.objectAtIndex_unchecked(1) };
            std::ptr::eq(browser, view).into()
        }

        #[unsafe(method(splitView:constrainSplitPosition:ofSubviewAt:))]
        fn split_view_constrain_split_position(
            &self,
            split_view: &NSSplitView,
            proposed_position: f64,
            divider_index: NSInteger,
        ) -> f64 {
            let width = split_view.bounds().size.width;
            let divider_width = 1.0;
            match divider_index {
                0 => {
                    let max_left = width
                        - RIGHT_SIDEBAR_MIN_WIDTH
                        - BROWSER_MIN_WIDTH
                        - (divider_width * 2.0);
                    proposed_position.clamp(LEFT_SIDEBAR_MIN_WIDTH, max_left.max(LEFT_SIDEBAR_MIN_WIDTH))
                }
                1 => {
                    let min_right_divider =
                        LEFT_SIDEBAR_MIN_WIDTH + BROWSER_MIN_WIDTH + divider_width;
                    let max_right_divider = width - RIGHT_SIDEBAR_MIN_WIDTH;
                    proposed_position.clamp(
                        min_right_divider.min(max_right_divider),
                        max_right_divider,
                    )
                }
                _ => proposed_position,
            }
        }
    }
    unsafe impl NSControlTextEditingDelegate for Delegate {}

    unsafe impl NSFilePromiseProviderDelegate for Delegate {
        #[unsafe(method(filePromiseProvider:fileNameForType:))]
        fn promise_file_name(
            &self,
            file_promise_provider: &NSFilePromiseProvider,
            _file_type: &NSString,
        ) -> *mut NSString {
            let Some(index) = self.file_promise_index(file_promise_provider) else {
                return Retained::autorelease_return(NSString::from_str("MacMTP Item"));
            };
            let name = self
                .ivars()
                .nodes
                .borrow()
                .get(index)
                .map(|node| sanitize_filename(&node.name))
                .unwrap_or_else(|| "MacMTP Item".to_string());
            Retained::autorelease_return(NSString::from_str(&name))
        }

        #[unsafe(method(filePromiseProvider:writePromiseToURL:completionHandler:))]
        fn write_promise_to_url(
            &self,
            file_promise_provider: &NSFilePromiseProvider,
            url: &NSURL,
            completion_handler: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            if let Err(message) =
                self.start_file_promise_copy(file_promise_provider, url, completion_handler.copy())
            {
                self.set_message(&tr("Drag Copy Failed"), &message);
                let error = promise_error(&message);
                completion_handler.call((Retained::autorelease_return(error),));
            }
        }

        #[unsafe(method(operationQueueForFilePromiseProvider:))]
        fn promise_operation_queue(
            &self,
            _file_promise_provider: &NSFilePromiseProvider,
        ) -> *mut NSOperationQueue {
            Retained::autorelease_return(NSOperationQueue::mainQueue())
        }
    }

    impl Delegate {
        #[unsafe(method(outlineView:numberOfChildrenOfItem:))]
        fn outline_number_of_children(
            &self,
            _outline_view: &NSOutlineView,
            item: Option<&AnyObject>,
        ) -> NSInteger {
            match self.item_index(item) {
                Some(index) => self.ivars().nodes.borrow()[index].children.len() as NSInteger,
                None => self.ivars().root_children.borrow().len() as NSInteger,
            }
        }

        #[unsafe(method(outlineView:child:ofItem:))]
        fn outline_child(
            &self,
            _outline_view: &NSOutlineView,
            index: NSInteger,
            item: Option<&AnyObject>,
        ) -> *mut AnyObject {
            let nodes = self.ivars().nodes.borrow();
            let roots = self.ivars().root_children.borrow();
            let children = match self.item_index(item) {
                Some(parent) => &nodes[parent].children,
                None => &roots,
            };
            let node_index = children[index as usize];
            let object: Retained<AnyObject> =
                NSNumber::new_usize(node_index).into_super().into_super().into();
            Retained::autorelease_return(object)
        }

        #[unsafe(method(outlineView:isItemExpandable:))]
        fn outline_is_expandable(
            &self,
            _outline_view: &NSOutlineView,
            item: &AnyObject,
        ) -> bool {
            self.item_index(Some(item))
                .and_then(|index| self.ivars().nodes.borrow().get(index).cloned())
                .is_some_and(|node| node.can_expand)
        }

        #[unsafe(method(outlineView:shouldExpandItem:))]
        fn outline_should_expand_item(&self, outline_view: &NSOutlineView, item: &AnyObject) -> bool {
            if self.reject_mtp_while_copying(&tr("Files are copying. Directories cannot be read right now.")) {
                return false.into();
            }
            if let Some(index) = self.item_index(Some(item)) {
                self.load_children(index);
                unsafe { outline_view.reloadItem_reloadChildren(Some(item), true) };
            }
            true
        }

        #[unsafe(method(outlineView:viewForTableColumn:item:))]
        fn outline_view_for_item(
            &self,
            _outline_view: &NSOutlineView,
            _table_column: Option<&NSTableColumn>,
            item: &AnyObject,
        ) -> *mut NSView {
            let Some(node) = self
                .item_index(Some(item))
                .and_then(|index| self.ivars().nodes.borrow().get(index).cloned())
            else {
                return std::ptr::null_mut();
            };

            let column = _table_column
                .map(|column| column.identifier())
                .unwrap_or_else(|| NSString::from_str("name"));
            let column_id: &NSString = column.as_ref();
            let text = if column_id == ns_string!("kind") {
                node.kind.to_string()
            } else if column_id == ns_string!("size") {
                node.size.clone()
            } else if column_id == ns_string!("created") {
                format_mtp_datetime(node.created)
            } else if column_id == ns_string!("modified") {
                format_mtp_datetime(node.modified)
            } else {
                node.name.clone()
            };

            let width = _table_column.map(NSTableColumn::width).unwrap_or(320.0);
            let container = NSView::new(self.mtm());
            container.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(width, 24.0),
            ));

            let field = NSTextField::labelWithString(&NSString::from_str(&text), self.mtm());
            field.setFont(Some(&NSFont::systemFontOfSize(14.0)));
            field.setUsesSingleLineMode(true);
            field.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            if node.is_file() {
                field.setTextColor(Some(&NSColor::labelColor()));
            } else {
                field.setTextColor(Some(&NSColor::secondaryLabelColor()));
            }
            field.setFrame(NSRect::new(
                NSPoint::new(6.0, 2.0),
                NSSize::new((width - 12.0).max(0.0), 20.0),
            ));
            field.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
            container.addSubview(&field);
            Retained::autorelease_return(container)
        }

        #[unsafe(method(outlineViewSelectionDidChange:))]
        fn outline_selection_changed(&self, _notification: &NSNotification) {
            self.update_detail();
        }

        #[unsafe(method(showQuickLook:))]
        fn show_quick_look(&self, _sender: Option<&AnyObject>) {
            if self.reject_mtp_while_copying(&tr("Files are copying. Preview is unavailable right now.")) {
                return;
            }
            self.open_quick_look_panel();
        }

        #[unsafe(method(refreshDevices:))]
        fn refresh_devices_action(&self, _sender: Option<&AnyObject>) {
            if self.reject_mtp_while_copying(&tr("Files are copying. Devices cannot be refreshed right now.")) {
                return;
            }
            self.refresh_devices();
        }

        #[unsafe(method(selectDevice:))]
        fn select_device_action(&self, sender: Option<&AnyObject>) {
            if self.reject_mtp_while_copying(&tr("Files are copying. Devices cannot be switched right now.")) {
                return;
            }
            let Some(index) = self.sender_device_index(sender) else {
                return;
            };
            self.select_device_at_index(index, false);
        }

        #[unsafe(method(mountDevice:))]
        fn mount_device_action(&self, sender: Option<&AnyObject>) {
            if self.reject_mtp_while_copying(&tr("Files are copying. Devices cannot be mounted right now.")) {
                return;
            }
            let Some(index) = self.sender_device_index(sender) else {
                return;
            };
            self.mount_device_at_index(index);
        }

        #[unsafe(method(ejectDevice:))]
        fn eject_device_action(&self, sender: Option<&AnyObject>) {
            if self.reject_mtp_while_copying(&tr("Files are copying. Devices cannot be ejected right now.")) {
                return;
            }
            let Some(index) = self.sender_device_index(sender) else {
                return;
            };
            self.eject_device_at_index(index);
        }

        #[unsafe(method(drainCopyEvents:))]
        fn drain_copy_events_action(&self, _timer: &NSTimer) {
            self.drain_copy_events();
        }

        #[unsafe(method(acceptsPreviewPanelControl:))]
        fn accepts_preview_panel_control(&self, _panel: &AnyObject) -> bool {
            if self.ivars().active_copies.load(Ordering::SeqCst) > 0 {
                return false.into();
            }
            self.selected_file().is_some()
        }

        #[unsafe(method(beginPreviewPanelControl:))]
        fn begin_preview_panel_control(&self, panel: &AnyObject) {
            unsafe {
                let _: () = msg_send![panel, setDataSource: self];
                let _: () = msg_send![panel, setDelegate: self];
            }
        }

        #[unsafe(method(endPreviewPanelControl:))]
        fn end_preview_panel_control(&self, panel: &AnyObject) {
            unsafe {
                let _: () = msg_send![panel, setDataSource: Option::<&AnyObject>::None];
                let _: () = msg_send![panel, setDelegate: Option::<&AnyObject>::None];
            }
        }

        #[unsafe(method(numberOfPreviewItemsInPreviewPanel:))]
        fn number_of_preview_items(&self, _panel: &AnyObject) -> NSInteger {
            if self.selected_file().is_some() { 1 } else { 0 }
        }

        #[unsafe(method(previewPanel:previewItemAtIndex:))]
        fn preview_item_at_index(
            &self,
            _panel: &AnyObject,
            _index: NSInteger,
        ) -> *mut NSURL {
            let Some(path) = self.prepare_selected_file_for_preview() else {
                return std::ptr::null_mut();
            };
            let ns_path = NSString::from_str(&path.to_string_lossy());
            Retained::autorelease_return(NSURL::fileURLWithPath(&ns_path))
        }

        #[unsafe(method(previewPanel:handleEvent:))]
        fn preview_panel_handle_event(&self, panel: &AnyObject, event: &NSEvent) -> bool {
            match event.keyCode() {
                125 => self.select_preview_file_relative(panel, 1),
                126 => self.select_preview_file_relative(panel, -1),
                _ => false,
            }
        }

        #[unsafe(method(outlineView:pasteboardWriterForItem:))]
        fn outline_pasteboard_writer_for_item(
            &self,
            _outline_view: &NSOutlineView,
            item: &AnyObject,
        ) -> *mut AnyObject {
            let Some(index) = self.item_index(Some(item)) else {
                return std::ptr::null_mut();
            };
            if !self.is_drag_copyable(index) {
                return std::ptr::null_mut();
            }

            let Some(provider) = self.file_promise_provider(index) else {
                return std::ptr::null_mut();
            };
            let object: Retained<AnyObject> = provider.into_super().into();
            Retained::autorelease_return(object)
        }

        #[unsafe(method(outlineView:draggingSession:willBeginAtPoint:forItems:))]
        fn outline_drag_will_begin(
            &self,
            _outline_view: &NSOutlineView,
            session: &NSDraggingSession,
            _screen_point: NSPoint,
            dragged_items: &NSArray,
        ) {
            let count = dragged_items.len();
            self.set_message(
                &tr("Ready to Drag Copy"),
                &tr("Prepared {count} file promises. Release the mouse to start copying.")
                    .replace("{count}", &count.to_string()),
            );
            let _ = session.draggingPasteboard();
        }

        #[allow(deprecated)]
        #[unsafe(method(outlineView:writeItems:toPasteboard:))]
        fn outline_write_items_to_pasteboard(
            &self,
            _outline_view: &NSOutlineView,
            items: &NSArray,
            pasteboard: &NSPasteboard,
        ) -> bool {
            self.write_drag_promises(items, pasteboard)
        }

        #[unsafe(method(outlineView:draggingSession:endedAtPoint:operation:))]
        fn outline_drag_ended(
            &self,
            _outline_view: &NSOutlineView,
            _session: &AnyObject,
            _screen_point: NSPoint,
            operation: NSDragOperation,
        ) {
            if operation == NSDragOperation::None {
                self.set_message(&tr("Drag Cancelled"), &tr("No files were copied."));
            }
        }

    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars::default());
        unsafe { msg_send![super(this), init] }
    }

    fn show_initial_device_prompt(&self) {
        self.render_device_rows();
        self.set_message(
            &tr("Select a Device"),
            &tr("The left device list scans MTP devices on launch. You can also click Refresh."),
        );
    }

    fn clear_browser_state(&self) {
        self.ivars().nodes.borrow_mut().clear();
        self.ivars().root_children.borrow_mut().clear();
        self.reload_outline();
        self.update_detail();
    }

    fn set_browser_message(&self, title: &str, detail: &str) {
        *self.ivars().nodes.borrow_mut() = vec![message_node(title, detail)];
        *self.ivars().root_children.borrow_mut() = vec![0];
        self.reload_outline();
        self.set_message(title, detail);
    }

    fn close_current_device(&self) {
        self.eject_current_mount();
        self.ivars().current_mount_location.borrow_mut().take();
        self.ivars().current_mounting_location.borrow_mut().take();
        self.ivars().pending_mount_location.borrow_mut().take();
        let device = self.ivars().device.borrow_mut().take();
        let mtp_lock = self.ivars().current_mtp_lock.borrow_mut().take();
        self.ivars().current_device_location.borrow_mut().take();
        if let Some(device) = device {
            let _ = self.with_mtp_lock(mtp_lock.as_ref(), || {
                self.runtime().block_on(async {
                    device
                        .session()
                        .execute(OperationCode::CloseSession, &[])
                        .await
                })
            });
        }
        self.update_mount_controls();
    }

    fn eject_current_mount(&self) {
        let Some(mount) = self.ivars().current_mount.borrow_mut().take() else {
            return;
        };
        let path = mount.mountpoint().to_string_lossy().to_string();
        let workspace = NSWorkspace::sharedWorkspace();
        let _ = workspace.unmountAndEjectDeviceAtPath(&NSString::from_str(&path));
        drop(mount);
    }

    fn item_index(&self, item: Option<&AnyObject>) -> Option<usize> {
        item.and_then(|item| item.downcast_ref::<NSNumber>())
            .map(NSNumber::as_usize)
    }

    fn drag_item_index(&self, item: &AnyObject) -> Option<usize> {
        if let Some(index) = self.item_index(Some(item)) {
            return Some(index);
        }

        let marker = item.downcast_ref::<NSString>()?.to_string();
        marker
            .strip_prefix(DRAG_NODE_PREFIX)
            .and_then(|index| index.parse().ok())
    }

    fn selected_node_index(&self) -> Option<usize> {
        let outline = self.ivars().outline_view.get()?;
        let row = outline.selectedRow();
        if row < 0 {
            return None;
        }
        let item = outline.itemAtRow(row)?;
        self.item_index(Some(&item))
    }

    fn selected_node(&self) -> Option<BrowserNode> {
        let index = self.selected_node_index()?;
        self.ivars().nodes.borrow().get(index).cloned()
    }

    fn selected_file(&self) -> Option<BrowserNode> {
        self.selected_node().filter(BrowserNode::is_file)
    }

    fn selected_file_row(&self) -> Option<NSInteger> {
        let outline = self.ivars().outline_view.get()?;
        let row = outline.selectedRow();
        if row < 0 {
            return None;
        }
        self.node_index_at_row(row)
            .and_then(|index| self.ivars().nodes.borrow().get(index).cloned())
            .filter(BrowserNode::is_file)
            .map(|_| row)
    }

    fn node_index_at_row(&self, row: NSInteger) -> Option<usize> {
        let outline = self.ivars().outline_view.get()?;
        if row < 0 || row >= outline.numberOfRows() {
            return None;
        }
        let item = outline.itemAtRow(row)?;
        self.item_index(Some(&item))
    }

    fn is_file_row(&self, row: NSInteger) -> bool {
        self.node_index_at_row(row)
            .and_then(|index| self.ivars().nodes.borrow().get(index).cloned())
            .is_some_and(|node| node.is_file())
    }

    fn select_preview_file_relative(&self, panel: &AnyObject, direction: NSInteger) -> bool {
        let Some(outline) = self.ivars().outline_view.get() else {
            return false;
        };
        let Some(current_row) = self.selected_file_row() else {
            return false;
        };
        let row_count = outline.numberOfRows();
        let mut row = current_row + direction;
        while row >= 0 && row < row_count {
            if self.is_file_row(row) {
                let indexes = NSIndexSet::indexSetWithIndex(row as usize);
                outline.selectRowIndexes_byExtendingSelection(&indexes, false);
                outline.scrollRowToVisible(row);
                unsafe {
                    let _: () = msg_send![panel, reloadData];
                }
                return true;
            }
            row += direction;
        }
        false
    }

    fn node_indexes_from_items(&self, items: &NSArray) -> Vec<usize> {
        let mut indexes = Vec::new();
        for index in 0..items.len() {
            let item = unsafe { items.objectAtIndex_unchecked(index) };
            if let Some(node_index) = self.drag_item_index(item) {
                indexes.push(node_index);
            }
        }
        indexes
    }

    fn is_drag_copyable(&self, index: usize) -> bool {
        self.ivars()
            .nodes
            .borrow()
            .get(index)
            .is_some_and(|node| node.is_file() || node.is_folder())
    }

    fn file_promise_provider(&self, index: usize) -> Option<Retained<NSFilePromiseProvider>> {
        let file_type = {
            let nodes = self.ivars().nodes.borrow();
            let node = nodes.get(index)?;
            if node.is_folder() {
                FILE_PROMISE_TYPE_FOLDER
            } else if node.is_file() {
                FILE_PROMISE_TYPE_FILE
            } else {
                return None;
            }
        };

        let provider = NSFilePromiseProvider::initWithFileType_delegate(
            NSFilePromiseProvider::alloc(),
            &NSString::from_str(file_type),
            ProtocolObject::from_ref(self),
        );
        let marker: Retained<AnyObject> = NSString::from_str(&format!("{DRAG_NODE_PREFIX}{index}"))
            .into_super()
            .into();
        unsafe { provider.setUserInfo(Some(&marker)) };
        Some(provider)
    }

    fn file_promise_index(&self, provider: &NSFilePromiseProvider) -> Option<usize> {
        let user_info = provider.userInfo()?;
        let marker = user_info.downcast::<NSString>().ok()?;
        marker
            .to_string()
            .strip_prefix(DRAG_NODE_PREFIX)
            .and_then(|index| index.parse().ok())
    }

    fn update_detail(&self) {
        let selected_device = self.selected_device_index().and_then(|index| {
            self.ivars()
                .devices
                .borrow()
                .get(index)
                .map(|device| (index, device.clone()))
        });
        let (title, detail, rows) = match self.selected_node() {
            Some(node) if node.is_file() => (
                node.name.to_string(),
                node.note.clone(),
                vec![
                    (tr("Kind"), node.kind),
                    (tr("Size"), node.size),
                    (tr("Created"), format_mtp_datetime(node.created)),
                    (tr("Modified"), format_mtp_datetime(node.modified)),
                ],
            ),
            Some(node) if node.is_folder() => (
                node.name.to_string(),
                node.note.clone(),
                vec![
                    (tr("Kind"), node.kind),
                    (
                        tr("Items"),
                        tr("{count} items").replace("{count}", &node.children.len().to_string()),
                    ),
                    (tr("Created"), format_mtp_datetime(node.created)),
                    (tr("Modified"), format_mtp_datetime(node.modified)),
                ],
            ),
            Some(node) => (
                node.name.to_string(),
                node.note.clone(),
                vec![
                    (tr("Kind"), node.kind),
                    (
                        tr("Items"),
                        tr("{count} items").replace("{count}", &node.children.len().to_string()),
                    ),
                ],
            ),
            None => match selected_device.as_ref() {
                Some((_index, device)) => (
                    self.device_list_name(device),
                    String::new(),
                    vec![
                        (tr("Status"), self.mount_status(device.location_id)),
                        (tr("Manufacturer"), self.device_manufacturer(device)),
                        (
                            tr("Serial Number"),
                            device
                                .serial_number
                                .as_deref()
                                .unwrap_or(&tr("Not Provided"))
                                .to_string(),
                        ),
                        (tr("Location"), format!("{:08x}", device.location_id)),
                    ],
                ),
                None => (
                    tr("No File Selected"),
                    tr("Select an MTP device, then expand a directory."),
                    Vec::new(),
                ),
            },
        };

        if let Some(label) = self.ivars().title_label.get() {
            label.setStringValue(&NSString::from_str(&title));
        }
        if let Some(label) = self.ivars().detail_label.get() {
            label.setStringValue(&NSString::from_str(&detail));
            label.setHidden(detail.is_empty());
        }
        self.render_detail_info_rows(rows);
        self.update_detail_device_controls(
            selected_device.map(|(index, device)| (index, device.location_id)),
        );
    }

    fn open_quick_look_panel(&self) {
        if self.selected_file().is_none() {
            return;
        }

        let Some(panel_class) = AnyClass::get(c"QLPreviewPanel") else {
            eprintln!("QLPreviewPanel is unavailable");
            return;
        };

        unsafe {
            let panel: *mut AnyObject = msg_send![panel_class, sharedPreviewPanel];
            if panel.is_null() {
                return;
            }
            let _: () = msg_send![panel, setDataSource: self];
            let _: () = msg_send![panel, setDelegate: self];
            let _: () = msg_send![panel, reloadData];
            let _: () = msg_send![panel, makeKeyAndOrderFront: Option::<&AnyObject>::None];
        }
    }

    fn refresh_devices(&self) {
        let result = MtpDevice::list_devices();
        let current_location = *self.ivars().current_device_location.borrow();

        match result {
            Err(err) => {
                self.ivars().devices.borrow_mut().clear();
                self.set_message(&tr("Device Scan Failed"), &format!("{err}"));
            }
            Ok(found) if found.is_empty() => {
                self.ivars().devices.borrow_mut().clear();
                if current_location.is_some() {
                    self.close_current_device();
                    self.clear_browser_state();
                }
                self.set_message(
                    &tr("No MTP Devices Found"),
                    &tr("Connect an Android, Kindle, or other MTP device, then click Refresh."),
                );
            }
            Ok(found) => {
                if let Some(current_location) = current_location {
                    if !found
                        .iter()
                        .any(|device| device.location_id == current_location)
                    {
                        self.close_current_device();
                        self.clear_browser_state();
                    }
                }
                *self.ivars().devices.borrow_mut() = found;
                if current_location.is_none() {
                    self.set_message(
                        &tr("Select a Device"),
                        &tr("Select an MTP device from the left device list."),
                    );
                }
            }
        }
        self.update_mount_controls();
    }

    fn select_device_at_index(&self, index: usize, mount_after_connect: bool) {
        let device_info = match self.ivars().devices.borrow().get(index) {
            Some(info) => info.clone(),
            None => return,
        };

        self.clear_outline_selection();

        if *self.ivars().current_device_location.borrow() == Some(device_info.location_id) {
            if mount_after_connect {
                if let Some(device) = self.ivars().device.borrow().clone() {
                    self.mount_current_device(device, &device_info);
                } else {
                    self.ivars()
                        .pending_mount_location
                        .replace(Some(device_info.location_id));
                }
            }
            self.update_detail();
            self.update_mount_controls();
            return;
        }

        self.close_current_device();
        let mtp_lock = self.mtp_lock_for_device(device_info.location_id);
        self.ivars()
            .current_device_location
            .replace(Some(device_info.location_id));
        self.ivars()
            .current_mtp_lock
            .replace(Some(mtp_lock.clone()));
        if mount_after_connect {
            self.ivars()
                .pending_mount_location
                .replace(Some(device_info.location_id));
        }
        self.set_browser_message(&tr("Connecting Device"), &device_info.display());
        self.update_detail();
        self.update_mount_controls();
        self.start_device_connect(device_info, mtp_lock);
    }

    fn start_device_connect(&self, device_info: MtpDeviceInfo, mtp_lock: Arc<Mutex<()>>) {
        let Some(tx) = self.ivars().device_events_tx.get().cloned() else {
            self.set_browser_message(
                &tr("Device Connection Failed"),
                &tr("Device event channel is not initialized."),
            );
            return;
        };
        self.update_mount_controls();
        thread::spawn(move || {
            let result = run_device_connect_worker(device_info.clone(), mtp_lock);
            let _ = tx.send(DeviceEvent::Connected {
                device_info,
                result,
            });
        });
    }

    fn apply_connected_device(
        &self,
        device_info: MtpDeviceInfo,
        device: MtpDevice,
        nodes: Vec<BrowserNode>,
        roots: Vec<usize>,
    ) {
        if *self.ivars().current_device_location.borrow() != Some(device_info.location_id) {
            let mtp_lock = self.mtp_lock_for_device(device_info.location_id);
            let _ = self.with_mtp_lock(Some(&mtp_lock), || {
                self.runtime().block_on(async {
                    device
                        .session()
                        .execute(OperationCode::CloseSession, &[])
                        .await
                })
            });
            return;
        }

        let should_mount =
            *self.ivars().pending_mount_location.borrow() == Some(device_info.location_id);
        let device_for_mount = if should_mount {
            Some(device.clone())
        } else {
            None
        };

        self.ivars().device.replace(Some(device));
        *self.ivars().nodes.borrow_mut() = nodes;
        *self.ivars().root_children.borrow_mut() = roots;
        self.reload_outline();
        self.update_detail();
        self.update_mount_controls();

        if let Some(device_for_mount) = device_for_mount {
            self.ivars().pending_mount_location.borrow_mut().take();
            self.mount_current_device(device_for_mount, &device_info);
            return;
        }

        self.set_message(
            &tr("Device Connected"),
            &tr("Browse files with the built-in browser, or use the right-side button to mount the device in the system."),
        );
        self.update_detail();
    }

    fn mount_device_at_index(&self, index: usize) {
        let Some(device_info) = self.ivars().devices.borrow().get(index).cloned() else {
            return;
        };
        let location_id = device_info.location_id;
        if *self.ivars().current_mount_location.borrow() == Some(location_id) {
            self.set_message(
                &tr("Already Mounted"),
                &tr("This device is already mounted."),
            );
            return;
        }
        if *self.ivars().current_mounting_location.borrow() == Some(location_id) {
            self.set_message(
                &tr("Mounting"),
                &tr("Wait for this device's mount operation to finish."),
            );
            return;
        }
        if *self.ivars().current_device_location.borrow() != Some(location_id) {
            self.select_device_at_index(index, true);
            return;
        }
        let Some(device) = self.ivars().device.borrow().clone() else {
            self.select_device_at_index(index, true);
            return;
        };
        self.mount_current_device(device, &device_info);
    }

    fn eject_device_at_index(&self, index: usize) {
        let Some(device_info) = self.ivars().devices.borrow().get(index).cloned() else {
            return;
        };
        if *self.ivars().current_mount_location.borrow() != Some(device_info.location_id) {
            self.set_message(
                &tr("Not Mounted"),
                &tr("This device is not currently mounted."),
            );
            return;
        }
        self.eject_current_mount();
        self.ivars().current_mount_location.borrow_mut().take();
        self.update_mount_controls();
        self.set_message(
            &tr("Ejected"),
            &tr("The built-in browser can still use the current device."),
        );
    }

    fn mount_current_device(&self, device: MtpDevice, device_info: &MtpDeviceInfo) {
        self.eject_current_mount();
        self.ivars().current_mount_location.borrow_mut().take();
        if !mount::macfuse_available() {
            self.set_message(
                &tr("Device Connected"),
                &tr("macFUSE was not detected, so built-in browser mode remains active. Install macFUSE to mount devices in the system."),
            );
            self.update_mount_controls();
            return;
        }

        let Some(tx) = self.ivars().mount_events_tx.get().cloned() else {
            self.set_message(
                &tr("Mount Failed"),
                &tr("Mount event channel is not initialized. The built-in browser remains available."),
            );
            return;
        };
        let device_info = device_info.clone();
        let location_id = device_info.location_id;
        let mtp_lock = self.mtp_lock_for_device(location_id);
        self.set_message(
            &tr("Mounting"),
            &tr("The built-in browser is available. Mounting will finish in the background."),
        );
        self.ivars()
            .current_mounting_location
            .replace(Some(location_id));
        self.update_mount_controls();
        thread::spawn(move || {
            let result = mount::mount_device(device, &device_info, mtp_lock);
            let _ = tx.send(MountEvent::Finished {
                location_id,
                result,
            });
        });
    }

    fn load_children(&self, index: usize) {
        if let Err(message) = self.load_children_result(index, None) {
            let mut nodes = self.ivars().nodes.borrow_mut();
            let child = nodes.len();
            nodes.push(message_node(&tr("Directory Read Failed"), &message));
            nodes[index].children = vec![child];
            nodes[index].children_loaded = true;
        }
    }

    fn load_children_result(&self, index: usize, timeout: Option<Duration>) -> Result<(), String> {
        let Some(device) = self.ivars().device.borrow().clone() else {
            return Err(tr("Device is not connected."));
        };
        if self
            .ivars()
            .nodes
            .borrow()
            .get(index)
            .is_none_or(|node| node.children_loaded)
        {
            return Ok(());
        }

        let (storage_id, parent) = {
            let nodes = self.ivars().nodes.borrow();
            match nodes.get(index).map(|node| &node.source) {
                Some(NodeSource::Storage { storage_id }) => (*storage_id, None),
                Some(NodeSource::Object {
                    storage_id,
                    handle,
                    is_folder: true,
                }) => (*storage_id, Some(*handle)),
                _ => return Ok(()),
            }
        };

        let result = self.with_mtp_lock(self.current_mtp_lock().as_ref(), || {
            self.runtime().block_on(async {
                let operation = async {
                    let storage = device.storage(storage_id).await?;
                    storage.list_objects(parent).await
                };
                match timeout {
                    Some(timeout) => tokio::time::timeout(timeout, operation)
                        .await
                        .map_err(|_| {
                            tr("MTP directory read exceeded {seconds} seconds.")
                                .replace("{seconds}", &timeout.as_secs().to_string())
                        })?
                        .map_err(|err| format_mtp_error(&err)),
                    None => operation.await.map_err(|err| format_mtp_error(&err)),
                }
            })
        });

        let objects = match result {
            Ok(Ok(objects)) => objects,
            Ok(Err(message)) => return Err(message),
            Err(message) => return Err(message),
        };

        let mut nodes = self.ivars().nodes.borrow_mut();
        let mut children = Vec::with_capacity(objects.len());
        for object in objects {
            let child = nodes.len();
            let is_folder = object.is_folder();
            children.push(child);
            nodes.push(BrowserNode {
                name: object.filename.clone(),
                kind: if is_folder { tr("Folder") } else { tr("File") },
                size: if is_folder {
                    "--".to_string()
                } else {
                    format_bytes(object.size)
                },
                created: object.created,
                modified: object.modified,
                note: tr("Select a file and press Space to preview it.\nDrag selected items to Finder to copy files to this Mac."),
                source: NodeSource::Object {
                    storage_id,
                    handle: object.handle,
                    is_folder,
                },
                children: Vec::new(),
                children_loaded: false,
                can_expand: is_folder,
                cached_path: None,
            });
        }
        nodes[index].children = children;
        nodes[index].children_loaded = true;
        Ok(())
    }

    fn prepare_selected_file_for_preview(&self) -> Option<PathBuf> {
        let index = self.selected_node_index()?;
        if let Some(path) = self.ivars().nodes.borrow()[index].cached_path.clone() {
            return Some(path);
        }

        let (storage_id, handle, name) = {
            let nodes = self.ivars().nodes.borrow();
            let node = nodes.get(index)?;
            match node.source {
                NodeSource::Object {
                    storage_id,
                    handle,
                    is_folder: false,
                } => (storage_id, handle, sanitize_filename(&node.name)),
                _ => return None,
            }
        };

        self.set_message(
            &tr("Preparing Preview"),
            &tr("Copying the file from the MTP device to a temporary directory."),
        );
        let device = self.ivars().device.borrow().clone()?;
        let result = self.with_mtp_lock(self.current_mtp_lock().as_ref(), || {
            self.runtime().block_on(async {
                let storage = device.storage(storage_id).await?;
                storage.download(handle).await
            })
        });
        let data = match result {
            Ok(Ok(data)) => data,
            Ok(Err(err)) => {
                self.set_message(&tr("Preview Failed"), &format_mtp_error(&err));
                return None;
            }
            Err(message) => {
                self.set_message(&tr("Preview Failed"), &message);
                return None;
            }
        };

        let path = std::env::temp_dir()
            .join("macmtp-quicklook")
            .join(format!("{}-{}", handle.0, name));
        if let Some(parent) = path.parent() {
            if fs::create_dir_all(parent).is_err() {
                return None;
            }
        }
        if fs::write(&path, data).is_err() {
            self.set_message(
                &tr("Preview Failed"),
                &tr("Unable to write the temporary preview file."),
            );
            return None;
        }
        self.ivars().nodes.borrow_mut()[index].cached_path = Some(path.clone());
        self.update_detail();
        Some(path)
    }

    fn write_drag_promises(&self, items: &NSArray, pasteboard: &NSPasteboard) -> bool {
        let indexes = self.node_indexes_from_items(items);
        if indexes.is_empty() {
            return false;
        }

        let promises: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = indexes
            .into_iter()
            .filter_map(|index| self.file_promise_provider(index))
            .map(ProtocolObject::from_retained)
            .collect();
        if promises.is_empty() {
            return false;
        }

        pasteboard.clearContents();
        let objects = NSArray::from_retained_slice(&promises);
        if pasteboard.writeObjects(&objects) {
            self.set_message(
                &tr("Ready to Drag Copy"),
                &tr("Prepared {count} file promises. Release the mouse to start copying.")
                    .replace("{count}", &promises.len().to_string()),
            );
            true
        } else {
            self.set_message(
                &tr("Drag Copy Failed"),
                &tr("Unable to write to the drag pasteboard."),
            );
            false
        }
    }

    fn start_file_promise_copy(
        &self,
        provider: &NSFilePromiseProvider,
        url: &NSURL,
        completion_handler: RcBlock<dyn Fn(*mut NSError)>,
    ) -> Result<(), String> {
        let index = self
            .file_promise_index(provider)
            .ok_or_else(|| tr("Drag item was not found."))?;
        let path = url
            .path()
            .map(|path| PathBuf::from(path.to_string()))
            .ok_or_else(|| tr("Finder did not provide a valid destination path."))?;
        let job = self
            .export_node(index)
            .ok_or_else(|| tr("Only MTP files or folders can be dragged."))?;
        let device = self
            .ivars()
            .device
            .borrow()
            .clone()
            .ok_or_else(|| tr("Device is not connected."))?;
        let tx = self
            .ivars()
            .copy_events_tx
            .get()
            .cloned()
            .ok_or_else(|| tr("Copy progress channel is not initialized."))?;
        let mtp_lock = self
            .current_mtp_lock()
            .ok_or_else(|| tr("Device operation lock is not initialized."))?;
        let active_copies = self.ivars().active_copies.clone();
        if active_copies.load(Ordering::SeqCst) == 0 {
            *self.ivars().copy_error.borrow_mut() = None;
        }
        active_copies.fetch_add(1, Ordering::SeqCst);

        self.set_message(
            &tr("Copying Dragged Items"),
            &tr("Copying from the MTP device to the destination."),
        );
        self.show_copy_progress(true);
        self.set_mtp_controls_enabled(false);
        let _ = tx.send(CopyEvent::Started);
        let completion_handler = SendCompletion(completion_handler);
        thread::spawn(move || {
            let result = run_copy_worker(device, mtp_lock, job, path, tx.clone());
            active_copies.fetch_sub(1, Ordering::SeqCst);
            let _ = tx.send(CopyEvent::Finished {
                result: result.clone(),
            });
            match result {
                Ok(()) => completion_handler.call_success(),
                Err(message) => completion_handler.call_error(&message),
            }
        });
        Ok(())
    }

    fn export_node(&self, index: usize) -> Option<ExportNode> {
        let nodes = self.ivars().nodes.borrow();
        let node = nodes.get(index)?;
        let NodeSource::Object {
            storage_id,
            handle,
            is_folder,
        } = node.source
        else {
            return None;
        };
        Some(ExportNode {
            name: node.name.clone(),
            storage_id,
            handle,
            is_folder,
        })
    }

    fn reject_mtp_while_copying(&self, detail: &str) -> bool {
        if self.ivars().active_copies.load(Ordering::SeqCst) == 0 {
            return false;
        }
        self.set_message(&tr("MTP Copying"), detail);
        true
    }

    fn runtime(&self) -> &Runtime {
        self.ivars().runtime.get().expect("runtime initialized")
    }

    fn mtp_lock_for_device(&self, location_id: u64) -> Arc<Mutex<()>> {
        self.ivars()
            .mtp_locks
            .borrow_mut()
            .entry(location_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn current_mtp_lock(&self) -> Option<Arc<Mutex<()>>> {
        self.ivars().current_mtp_lock.borrow().clone()
    }

    fn with_mtp_lock<T>(
        &self,
        mtp_lock: Option<&Arc<Mutex<()>>>,
        operation: impl FnOnce() -> T,
    ) -> Result<T, String> {
        let mtp_lock = mtp_lock.ok_or_else(|| tr("Device operation lock is not initialized."))?;
        let _guard = mtp_lock
            .lock()
            .map_err(|_| tr("MTP operation lock is poisoned."))?;
        Ok(operation())
    }

    fn reload_outline(&self) {
        if let Some(outline) = self.ivars().outline_view.get() {
            outline.reloadData();
        }
    }

    fn set_message(&self, title: &str, detail: &str) {
        if let Some(label) = self.ivars().title_label.get() {
            label.setStringValue(&NSString::from_str(title));
        }
        if let Some(label) = self.ivars().detail_label.get() {
            label.setStringValue(&NSString::from_str(detail));
        }
    }

    fn install_copy_event_timer(&self) {
        let (copy_tx, copy_rx) = mpsc::channel();
        self.ivars().copy_events_tx.set(copy_tx).ok();
        *self.ivars().copy_events_rx.borrow_mut() = Some(copy_rx);
        let (mount_tx, mount_rx) = mpsc::channel();
        self.ivars().mount_events_tx.set(mount_tx).ok();
        *self.ivars().mount_events_rx.borrow_mut() = Some(mount_rx);
        let (device_tx, device_rx) = mpsc::channel();
        self.ivars().device_events_tx.set(device_tx).ok();
        *self.ivars().device_events_rx.borrow_mut() = Some(device_rx);
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                0.1,
                self,
                sel!(drainCopyEvents:),
                None,
                true,
            )
        };
        self.ivars().copy_timer.set(timer).ok();
    }

    fn drain_copy_events(&self) {
        self.drain_device_events();
        self.drain_mount_events();
        let mut last_progress = None;
        let mut last_result = None;
        if let Some(rx) = self.ivars().copy_events_rx.borrow_mut().as_mut() {
            while let Ok(event) = rx.try_recv() {
                match event {
                    CopyEvent::Started => {
                        self.show_copy_progress(true);
                        self.set_mtp_controls_enabled(false);
                    }
                    CopyEvent::Progress {
                        name,
                        bytes_done,
                        bytes_total,
                        files_done,
                    } => {
                        last_progress = Some((name, bytes_done, bytes_total, files_done));
                    }
                    CopyEvent::Finished { result } => {
                        last_result = Some(result);
                    }
                }
            }
        }

        if let Some((name, bytes_done, bytes_total, files_done)) = last_progress {
            self.update_copy_progress(&name, bytes_done, bytes_total, files_done);
        }
        if let Some(result) = last_result {
            if let Err(message) = &result {
                *self.ivars().copy_error.borrow_mut() = Some(message.clone());
            }
            if self.ivars().active_copies.load(Ordering::SeqCst) == 0 {
                self.show_copy_progress(false);
                self.set_mtp_controls_enabled(true);
                if let Some(message) = self.ivars().copy_error.borrow_mut().take() {
                    self.set_message(&tr("Drag Copy Failed"), &message);
                } else if result.is_ok() {
                    self.set_message(
                        &tr("Drag Copy Complete"),
                        &tr("Files were copied to the destination."),
                    );
                }
            }
        }
    }

    fn drain_device_events(&self) {
        let mut last_result = None;
        if let Some(rx) = self.ivars().device_events_rx.borrow_mut().as_mut() {
            while let Ok(event) = rx.try_recv() {
                match event {
                    DeviceEvent::Connected {
                        device_info,
                        result,
                    } => {
                        last_result = Some((device_info, result));
                    }
                }
            }
        }

        let Some((device_info, result)) = last_result else {
            return;
        };
        if *self.ivars().current_device_location.borrow() != Some(device_info.location_id) {
            return;
        }

        match result {
            Ok((device, nodes, roots)) => {
                self.apply_connected_device(device_info, device, nodes, roots);
            }
            Err(message) => {
                self.ivars().device.borrow_mut().take();
                self.ivars().current_device_location.borrow_mut().take();
                self.ivars().current_mtp_lock.borrow_mut().take();
                self.update_mount_controls();
                self.set_browser_message(&tr("Device Connection Failed"), &message);
            }
        }
    }

    fn drain_mount_events(&self) {
        let mut last_result = None;
        if let Some(rx) = self.ivars().mount_events_rx.borrow_mut().as_mut() {
            while let Ok(event) = rx.try_recv() {
                match event {
                    MountEvent::Finished {
                        location_id,
                        result,
                    } => {
                        last_result = Some((location_id, result));
                    }
                }
            }
        }

        let Some((location_id, result)) = last_result else {
            return;
        };
        if *self.ivars().current_device_location.borrow() != Some(location_id) {
            if let Ok(handle) = result {
                drop(handle);
            }
            if *self.ivars().current_mounting_location.borrow() == Some(location_id) {
                self.ivars().current_mounting_location.borrow_mut().take();
                self.update_mount_controls();
            }
            return;
        }
        self.ivars().current_mounting_location.borrow_mut().take();

        match result {
            Ok(handle) => {
                let path = handle.mountpoint().display().to_string();
                self.ivars().current_mount.replace(Some(handle));
                self.ivars()
                    .current_mount_location
                    .replace(Some(location_id));
                self.update_mount_controls();
                self.set_message(
                    &tr("Mounted in Finder"),
                    &tr("The device is mounted at {path}. It will be ejected automatically before quitting.")
                        .replace("{path}", &path),
                );
            }
            Err(message) => {
                self.update_mount_controls();
                self.set_message(
                    &tr("Finder Mount Failed"),
                    &format!(
                        "{}\n{}",
                        message,
                        tr("The built-in browser remains available.")
                    ),
                );
            }
        }
    }

    fn update_copy_progress(
        &self,
        name: &str,
        bytes_done: u64,
        bytes_total: Option<u64>,
        files_done: usize,
    ) {
        if let Some(progress) = self.ivars().progress_indicator.get() {
            match bytes_total {
                Some(total) if total > 0 => {
                    progress.setIndeterminate(false);
                    progress.setDoubleValue((bytes_done as f64 / total as f64) * 100.0);
                }
                _ => {
                    progress.setIndeterminate(true);
                    unsafe { progress.startAnimation(None) };
                }
            }
        }
        let detail = match bytes_total {
            Some(total) if total > 0 => format!(
                "{}\n{}",
                name,
                tr("Completed {done} / {total}, {files} files total.")
                    .replace("{done}", &format_bytes(bytes_done))
                    .replace("{total}", &format_bytes(total))
                    .replace("{files}", &files_done.to_string())
            ),
            _ => format!(
                "{}\n{}",
                name,
                tr("Copied {files} files.").replace("{files}", &files_done.to_string())
            ),
        };
        self.set_message(&tr("Copying Dragged Items"), &detail);
    }

    fn show_copy_progress(&self, visible: bool) {
        if let Some(progress) = self.ivars().progress_indicator.get() {
            progress.setHidden(!visible);
            if visible {
                progress.setDoubleValue(0.0);
            } else {
                progress.setIndeterminate(false);
                progress.setDoubleValue(0.0);
                unsafe { progress.stopAnimation(None) };
            }
        }
    }

    fn set_mtp_controls_enabled(&self, enabled: bool) {
        if let Some(button) = self.ivars().refresh_button.get() {
            button.setEnabled(enabled);
        }
        if let Some(outline) = self.ivars().outline_view.get() {
            outline.setEnabled(enabled);
        }
        if enabled {
            self.update_mount_controls();
        } else {
            self.render_device_rows();
        }
    }

    fn update_mount_controls(&self) {
        self.update_detail_device_controls(self.selected_device_index().and_then(|index| {
            self.ivars()
                .devices
                .borrow()
                .get(index)
                .map(|device| (index, device.location_id))
        }));
        self.render_device_rows();
    }

    fn render_detail_info_rows(&self, rows: Vec<(String, String)>) {
        let Some(info_view) = self.ivars().detail_info_view.get() else {
            return;
        };

        for row in self.ivars().detail_info_rows.borrow_mut().drain(..) {
            row.removeFromSuperview();
        }

        if rows.is_empty() {
            return;
        }

        let bounds = info_view.bounds();
        let width = bounds.size.width.max(0.0);
        let header_height = 24.0;
        let row_height = 30.0;
        let mut y = (bounds.size.height - header_height).max(0.0);

        let header = NSTextField::labelWithString(&ns_tr("Info"), self.mtm());
        header.setFrame(NSRect::new(
            NSPoint::new(0.0, y),
            NSSize::new(width, header_height),
        ));
        header.setFont(Some(&NSFont::boldSystemFontOfSize(14.0)));
        header.setTextColor(Some(&NSColor::labelColor()));
        header.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        info_view.addSubview(&header);
        let header_view: Retained<NSView> = header.into_super().into_super();
        self.ivars().detail_info_rows.borrow_mut().push(header_view);

        y -= row_height;
        for (label, value) in rows {
            let row = NSView::new(self.mtm());
            row.setFrame(NSRect::new(
                NSPoint::new(0.0, y.max(0.0)),
                NSSize::new(width, row_height),
            ));
            row.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewMinYMargin,
            );

            let key_field = NSTextField::labelWithString(&NSString::from_str(&label), self.mtm());
            key_field.setFrame(NSRect::new(NSPoint::new(0.0, 6.0), NSSize::new(76.0, 18.0)));
            key_field.setFont(Some(&NSFont::systemFontOfSize(12.0)));
            key_field.setTextColor(Some(&NSColor::secondaryLabelColor()));
            key_field.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);

            let value_field =
                NSTextField::labelWithString(&NSString::from_str(value.trim()), self.mtm());
            value_field.setFrame(NSRect::new(
                NSPoint::new(78.0, 6.0),
                NSSize::new((width - 78.0).max(0.0), 18.0),
            ));
            value_field.setFont(Some(&NSFont::systemFontOfSize(12.0)));
            value_field.setTextColor(Some(&NSColor::labelColor()));
            value_field.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
            value_field.setAlignment(NSTextAlignment::Right);
            value_field.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);

            let separator = NSBox::initWithFrame(
                NSBox::alloc(self.mtm()),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, 1.0)),
            );
            separator.setBoxType(NSBoxType::Separator);
            separator.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);

            row.addSubview(&separator);
            row.addSubview(&key_field);
            row.addSubview(&value_field);
            info_view.addSubview(&row);
            self.ivars().detail_info_rows.borrow_mut().push(row);

            y -= row_height;
            if y < 0.0 {
                break;
            }
        }
    }

    fn selected_device_index(&self) -> Option<usize> {
        let location = *self.ivars().current_device_location.borrow();
        let devices = self.ivars().devices.borrow();
        location.and_then(|location| {
            devices
                .iter()
                .position(|device| device.location_id == location)
        })
    }

    fn clear_outline_selection(&self) {
        if let Some(outline) = self.ivars().outline_view.get() {
            unsafe { outline.deselectAll(None) };
        }
    }

    fn device_list_name(&self, device: &MtpDeviceInfo) -> String {
        device
            .product
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&tr("MTP Device"))
            .to_string()
    }

    fn device_manufacturer(&self, device: &MtpDeviceInfo) -> String {
        device
            .manufacturer
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&tr("Not Provided"))
            .to_string()
    }

    fn mount_status(&self, location_id: u64) -> String {
        if *self.ivars().current_mount_location.borrow() == Some(location_id) {
            tr("Mounted in Finder")
        } else if *self.ivars().current_mounting_location.borrow() == Some(location_id) {
            tr("Mounting in Finder")
        } else if *self.ivars().current_device_location.borrow() == Some(location_id) {
            tr("Connected, Not Mounted")
        } else {
            tr("Not Connected")
        }
    }

    fn update_detail_device_controls(&self, selected: Option<(usize, u64)>) {
        let controls_enabled = self.ivars().active_copies.load(Ordering::SeqCst) == 0;
        let mounted_location = *self.ivars().current_mount_location.borrow();
        let mounting_location = *self.ivars().current_mounting_location.borrow();

        if let Some(button) = self.ivars().detail_mount_button.get() {
            let tag = selected
                .map(|(index, _)| (index + 1) as NSInteger)
                .unwrap_or(0);
            button.setTag(tag);
            button.setEnabled(
                controls_enabled
                    && selected.is_some_and(|(_, location_id)| {
                        mounted_location != Some(location_id)
                            && mounting_location != Some(location_id)
                    }),
            );
        }
        if let Some(button) = self.ivars().detail_eject_button.get() {
            let tag = selected
                .map(|(index, _)| (index + 1) as NSInteger)
                .unwrap_or(0);
            button.setTag(tag);
            button.setEnabled(
                controls_enabled
                    && selected
                        .is_some_and(|(_, location_id)| mounted_location == Some(location_id)),
            );
        }
    }

    fn sender_device_index(&self, sender: Option<&AnyObject>) -> Option<usize> {
        let tag = sender?.downcast_ref::<NSButton>()?.tag();
        if tag <= 0 {
            return None;
        }
        Some((tag - 1) as usize)
    }

    fn render_device_rows(&self) {
        let Some(device_list) = self.ivars().device_list_view.get() else {
            return;
        };

        for row in self.ivars().device_row_views.borrow_mut().drain(..) {
            row.removeFromSuperview();
        }

        let bounds = device_list.bounds();
        let list_width = bounds.size.width.max(0.0);
        let list_height = bounds.size.height.max(0.0);
        let row_height = 54.0;
        let row_step = 58.0;
        let top_y = (list_height - row_height).max(0.0);
        let text_x = 40.0;
        let title_width = (list_width - text_x - 12.0).max(0.0);

        let devices = self.ivars().devices.borrow();
        if devices.is_empty() {
            let row = NSView::new(self.mtm());
            row.setFrame(NSRect::new(
                NSPoint::new(0.0, top_y),
                NSSize::new(list_width, 30.0),
            ));
            row.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewMinYMargin
                    | NSAutoresizingMaskOptions::ViewWidthSizable,
            );
            let label = NSTextField::labelWithString(&ns_tr("No MTP Devices Found"), self.mtm());
            label.setFrame(NSRect::new(
                NSPoint::new(6.0, 5.0),
                NSSize::new((list_width - 12.0).max(0.0), 20.0),
            ));
            label.setFont(Some(&NSFont::systemFontOfSize(13.0)));
            label.setTextColor(Some(&NSColor::secondaryLabelColor()));
            label.setUsesSingleLineMode(true);
            label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            label.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
            row.addSubview(&label);
            device_list.addSubview(&row);
            self.ivars().device_row_views.borrow_mut().push(row);
            return;
        }

        let current_location = *self.ivars().current_device_location.borrow();
        let controls_enabled = self.ivars().active_copies.load(Ordering::SeqCst) == 0;

        for (index, device) in devices.iter().enumerate() {
            let y = (top_y - (index as f64 * row_step)).max(0.0);
            let device_row = DeviceRowView::new(self.mtm());
            let row: Retained<NSView> = device_row.clone().into_super();
            row.setFrame(NSRect::new(
                NSPoint::new(0.0, y),
                NSSize::new(list_width, row_height),
            ));
            row.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewMinYMargin
                    | NSAutoresizingMaskOptions::ViewWidthSizable,
            );

            let hover_background = NSBox::initWithFrame(
                NSBox::alloc(self.mtm()),
                NSRect::new(
                    NSPoint::new(0.0, 1.0),
                    NSSize::new(list_width, row_height - 2.0),
                ),
            );
            hover_background.setBoxType(NSBoxType::Custom);
            hover_background.setCornerRadius(6.0);
            hover_background.setTransparent(false);
            hover_background.setFillColor(&NSColor::separatorColor());
            hover_background.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
            row.addSubview(&hover_background);
            device_row.set_hover_background(hover_background);

            if current_location == Some(device.location_id) {
                let background = NSBox::initWithFrame(
                    NSBox::alloc(self.mtm()),
                    NSRect::new(
                        NSPoint::new(0.0, 1.0),
                        NSSize::new(list_width, row_height - 2.0),
                    ),
                );
                background.setBoxType(NSBoxType::Custom);
                background.setCornerRadius(6.0);
                background.setTransparent(false);
                background.setFillColor(&NSColor::unemphasizedSelectedContentBackgroundColor());
                background.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
                row.addSubview(&background);
            }

            let select_button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    ns_string!(""),
                    Some(self),
                    Some(sel!(selectDevice:)),
                    self.mtm(),
                )
            };
            select_button.setFrame(NSRect::new(
                NSPoint::new(0.0, 1.0),
                NSSize::new(list_width, row_height - 2.0),
            ));
            select_button.setBordered(false);
            select_button.setTransparent(true);
            select_button.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
            select_button.setTag((index + 1) as NSInteger);
            select_button.setEnabled(controls_enabled);
            device_row.install_hover_tracking();

            if let Some(icon) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                ns_string!("externaldrive"),
                Some(&ns_tr("MTP device")),
            ) {
                let image_view = NSImageView::imageViewWithImage(&icon, self.mtm());
                image_view.setFrame(NSRect::new(
                    NSPoint::new(10.0, 16.0),
                    NSSize::new(20.0, 20.0),
                ));
                image_view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMaxXMargin);
                row.addSubview(&image_view);
            }

            let title = self.device_list_name(device);
            let title_label = NSTextField::labelWithString(&NSString::from_str(&title), self.mtm());
            title_label.setFrame(NSRect::new(
                NSPoint::new(text_x, 26.0),
                NSSize::new(title_width, 18.0),
            ));
            title_label.setFont(Some(&NSFont::systemFontOfSize(13.0)));
            title_label.setUsesSingleLineMode(true);
            title_label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            title_label.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
            let title_color = if current_location == Some(device.location_id) {
                NSColor::labelColor()
            } else {
                NSColor::secondaryLabelColor()
            };
            title_label.setTextColor(Some(&title_color));

            let status_label = NSTextField::labelWithString(
                &NSString::from_str(&self.mount_status(device.location_id)),
                self.mtm(),
            );
            status_label.setFrame(NSRect::new(
                NSPoint::new(text_x, 9.0),
                NSSize::new(title_width, 16.0),
            ));
            status_label.setFont(Some(&NSFont::systemFontOfSize(12.0)));
            status_label.setUsesSingleLineMode(true);
            status_label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            status_label.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
            status_label.setTextColor(Some(&NSColor::secondaryLabelColor()));

            row.addSubview(&title_label);
            row.addSubview(&status_label);
            row.addSubview(&select_button);
            device_list.addSubview(&row);
            self.ivars().device_row_views.borrow_mut().push(row);
        }
    }
}

fn unique_child_path(parent: &PathBuf, name: &str) -> PathBuf {
    let mut candidate = parent.join(name);
    if !candidate.exists() {
        return candidate;
    }

    let path = std::path::Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("item");
    let extension = path.extension().and_then(|extension| extension.to_str());
    for suffix in 2.. {
        let filename = match extension {
            Some(extension) => format!("{stem} {suffix}.{extension}"),
            None => format!("{stem} {suffix}"),
        };
        candidate = parent.join(filename);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

fn run_copy_worker(
    device: MtpDevice,
    mtp_lock: Arc<Mutex<()>>,
    node: ExportNode,
    path: PathBuf,
    tx: mpsc::Sender<CopyEvent>,
) -> Result<(), String> {
    let _guard = mtp_lock
        .lock()
        .map_err(|_| tr("MTP operation lock is poisoned."))?;
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("{}: {err}", tr("Unable to create copy runtime")))?;
    let mut state = CopyState {
        files_done: 0,
        last_progress: Instant::now() - COPY_PROGRESS_THROTTLE,
        tx,
    };
    runtime.block_on(async { export_node_worker(&device, &node, &path, &mut state).await })
}

fn run_device_connect_worker(
    device_info: MtpDeviceInfo,
    mtp_lock: Arc<Mutex<()>>,
) -> Result<(MtpDevice, Vec<BrowserNode>, Vec<usize>), String> {
    let _guard = mtp_lock
        .lock()
        .map_err(|_| tr("MTP operation lock is poisoned."))?;
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            format!(
                "{}: {err}",
                tr("Unable to create device connection runtime")
            )
        })?;
    let device = runtime
        .block_on(MtpDevice::open_by_location(device_info.location_id))
        .map_err(|err| format_mtp_error(&err))?;
    let storages = runtime
        .block_on(async { device.storages().await })
        .map_err(|err| format_mtp_error(&err))?;
    let (nodes, roots) = storage_nodes(storages);
    Ok((device, nodes, roots))
}

fn storage_nodes(storages: Vec<mtp_rs::Storage>) -> (Vec<BrowserNode>, Vec<usize>) {
    let mut nodes = Vec::new();
    let mut roots = Vec::new();
    for storage in storages {
        let info = storage.info();
        let index = nodes.len();
        roots.push(index);
        nodes.push(BrowserNode {
            name: info.description.clone(),
            kind: tr("Storage"),
            size: format_bytes(info.free_space_bytes),
            created: None,
            modified: None,
            note: format!(
                "{}: {}\n{}: {}",
                tr("Storage ID"),
                storage.id().0,
                tr("Free Space"),
                format_bytes(info.free_space_bytes)
            ),
            source: NodeSource::Storage {
                storage_id: storage.id(),
            },
            children: Vec::new(),
            children_loaded: false,
            can_expand: true,
            cached_path: None,
        });
    }

    if roots.is_empty() {
        nodes.push(message_node(
            &tr("Device Has No Available Storage"),
            &tr("The MTP device did not return a storage list."),
        ));
        roots.push(0);
    }
    (nodes, roots)
}

struct CopyState {
    files_done: usize,
    last_progress: Instant,
    tx: mpsc::Sender<CopyEvent>,
}

async fn export_node_worker(
    device: &MtpDevice,
    node: &ExportNode,
    path: &PathBuf,
    state: &mut CopyState,
) -> Result<(), String> {
    if node.is_folder {
        export_folder_worker(device, node, path, state).await
    } else {
        export_file_worker(device, node, path, state).await
    }
}

async fn export_file_worker(
    device: &MtpDevice,
    node: &ExportNode,
    path: &PathBuf,
    state: &mut CopyState,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("{}: {err}", tr("Unable to create destination directory")))?;
    }

    let storage = device
        .storage(node.storage_id)
        .await
        .map_err(|err| format_mtp_error(&err))?;
    let mut download = storage
        .download_stream(node.handle)
        .await
        .map_err(|err| format_mtp_error(&err))?;
    let total = download.size();
    let mut file =
        fs::File::create(path).map_err(|err| format!("{}: {err}", tr("Unable to write file")))?;

    while let Some(chunk) = download.next_chunk().await {
        let chunk = chunk.map_err(|err| format_mtp_error(&err))?;
        file.write_all(&chunk)
            .map_err(|err| format!("{}: {err}", tr("Unable to write file")))?;
        if state.last_progress.elapsed() >= COPY_PROGRESS_THROTTLE {
            state.last_progress = Instant::now();
            let _ = state.tx.send(CopyEvent::Progress {
                name: node.name.clone(),
                bytes_done: download.bytes_received(),
                bytes_total: Some(total),
                files_done: state.files_done,
            });
        }
    }
    file.flush()
        .map_err(|err| format!("{}: {err}", tr("Unable to write file")))?;
    state.files_done += 1;
    let _ = state.tx.send(CopyEvent::Progress {
        name: node.name.clone(),
        bytes_done: total,
        bytes_total: Some(total),
        files_done: state.files_done,
    });
    Ok(())
}

async fn export_folder_worker(
    device: &MtpDevice,
    node: &ExportNode,
    path: &PathBuf,
    state: &mut CopyState,
) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|err| format!("{}: {err}", tr("Unable to create folder")))?;
    let _ = state.tx.send(CopyEvent::Progress {
        name: node.name.clone(),
        bytes_done: 0,
        bytes_total: None,
        files_done: state.files_done,
    });

    let storage = device
        .storage(node.storage_id)
        .await
        .map_err(|err| format_mtp_error(&err))?;
    let children = storage
        .list_objects(Some(node.handle))
        .await
        .map_err(|err| format_mtp_error(&err))?;

    for child in children {
        let is_folder = child.is_folder();
        let child_name = sanitize_filename(&child.filename);
        let child_path = unique_child_path(path, &child_name);
        let child_node = ExportNode {
            name: child.filename,
            storage_id: node.storage_id,
            handle: child.handle,
            is_folder,
        };
        Box::pin(export_node_worker(device, &child_node, &child_path, state)).await?;
    }
    Ok(())
}

fn promise_error(message: &str) -> Retained<NSError> {
    let _ = message;
    unsafe {
        NSError::errorWithDomain_code_userInfo(
            &NSString::from_str(FILE_PROMISE_ERROR_DOMAIN),
            1,
            None,
        )
    }
}

pub fn run() {
    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    let delegate = Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}
