use std::cell::{OnceCell, RefCell};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mtp_rs::mtp::{MtpDevice, MtpDeviceInfo};
use mtp_rs::{ObjectHandle, StorageId};
use objc2_quartz as _;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSColor, NSControlTextEditingDelegate, NSDragOperation, NSDraggingSession, NSFont,
    NSOutlineView, NSOutlineViewDataSource, NSOutlineViewDelegate, NSPasteboard,
    NSPasteboardWriting, NSPopUpButton, NSTableColumn, NSTextField, NSView, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSInteger, NSNotification, NSNumber, NSObject, NSObjectProtocol,
    NSPoint, NSRect, NSSize, NSString, NSURL, ns_string,
};
use tokio::runtime::{Builder, Runtime};

use crate::model::{BrowserNode, NodeSource, message_node};
use crate::ui::{build_browser_ui, install_main_menu};
use crate::util::{format_bytes, format_mtp_error, sanitize_filename};

const DRAG_NODE_PREFIX: &str = "macmtp-node:";
const DRAG_MTP_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Default)]
pub(crate) struct AppDelegateIvars {
    pub(crate) window: OnceCell<Retained<NSWindow>>,
    pub(crate) outline_view: OnceCell<Retained<NSOutlineView>>,
    pub(crate) device_popup: OnceCell<Retained<NSPopUpButton>>,
    pub(crate) title_label: OnceCell<Retained<NSTextField>>,
    pub(crate) detail_label: OnceCell<Retained<NSTextField>>,
    runtime: OnceCell<Runtime>,
    devices: RefCell<Vec<MtpDeviceInfo>>,
    device: RefCell<Option<MtpDevice>>,
    nodes: RefCell<Vec<BrowserNode>>,
    root_children: RefCell<Vec<usize>>,
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
            self.refresh_devices();

            window.center();
            window.makeKeyAndOrderFront(None);
            self.ivars().window.set(window).unwrap();

            self.update_detail();

            app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }
    }

    unsafe impl NSWindowDelegate for Delegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
    }

    unsafe impl NSOutlineViewDataSource for Delegate {}
    unsafe impl NSControlTextEditingDelegate for Delegate {}
    unsafe impl NSOutlineViewDelegate for Delegate {}

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
            } else {
                node.name.clone()
            };

            let field = NSTextField::labelWithString(&NSString::from_str(&text), self.mtm());
            field.setFont(Some(&NSFont::systemFontOfSize(14.0)));
            if node.is_file() {
                field.setTextColor(Some(&NSColor::labelColor()));
            } else {
                field.setTextColor(Some(&NSColor::secondaryLabelColor()));
            }
            field.setFrame(NSRect::new(
                NSPoint::new(6.0, 0.0),
                NSSize::new(320.0, 24.0),
            ));
            Retained::autorelease_return(field.into_super().into_super())
        }

        #[unsafe(method(outlineViewSelectionDidChange:))]
        fn outline_selection_changed(&self, _notification: &NSNotification) {
            self.update_detail();
        }

        #[unsafe(method(showQuickLook:))]
        fn show_quick_look(&self, _sender: Option<&AnyObject>) {
            self.open_quick_look_panel();
        }

        #[unsafe(method(refreshDevices:))]
        fn refresh_devices_action(&self, _sender: Option<&AnyObject>) {
            self.refresh_devices();
        }

        #[unsafe(method(selectDevice:))]
        fn select_device_action(&self, _sender: Option<&AnyObject>) {
            self.select_current_device();
        }

        #[unsafe(method(acceptsPreviewPanelControl:))]
        fn accepts_preview_panel_control(&self, _panel: &AnyObject) -> bool {
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

            let marker = NSString::from_str(&format!("{DRAG_NODE_PREFIX}{index}"));
            let object: Retained<AnyObject> = marker.into_super().into();
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
            let pasteboard = session.draggingPasteboard();
            self.write_drag_items(dragged_items, &pasteboard);
        }

        #[allow(deprecated)]
        #[unsafe(method(outlineView:writeItems:toPasteboard:))]
        fn outline_write_items_to_pasteboard(
            &self,
            _outline_view: &NSOutlineView,
            items: &NSArray,
            pasteboard: &NSPasteboard,
        ) -> bool {
            self.write_drag_items(items, pasteboard)
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
                self.set_message("拖拽已取消", "没有复制文件。");
            }
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars::default());
        unsafe { msg_send![super(this), init] }
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

    fn update_detail(&self) {
        let (title, detail) = match self.selected_node() {
            Some(node) if node.is_file() => (
                node.name.to_string(),
                format!("{}\n{}\n\n{}", node.kind, node.size, node.note),
            ),
            Some(node) => (
                node.name.to_string(),
                format!(
                    "{}\n{} 个项目\n\n{}",
                    node.kind,
                    node.children.len(),
                    node.note
                ),
            ),
            None => (
                "未选择文件".to_string(),
                "选择 MTP 设备后展开目录；选中文件按空格才会下载到临时目录并 Quick Look。"
                    .to_string(),
            ),
        };

        if let Some(label) = self.ivars().title_label.get() {
            label.setStringValue(&NSString::from_str(&title));
        }
        if let Some(label) = self.ivars().detail_label.get() {
            label.setStringValue(&NSString::from_str(&detail));
        }
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
        let mut devices = self.ivars().devices.borrow_mut();
        devices.clear();

        let Some(popup) = self.ivars().device_popup.get() else {
            return;
        };
        popup.removeAllItems();

        match result {
            Ok(found) if found.is_empty() => {
                popup.addItemWithTitle(ns_string!("未发现 MTP 设备"));
                self.set_message(
                    "未发现 MTP 设备",
                    "连接 Android/Kindle 等 MTP 设备后点击菜单 Device -> Refresh Devices。",
                );
            }
            Ok(found) => {
                popup.addItemWithTitle(ns_string!("选择 MTP 设备..."));
                for device in &found {
                    popup.addItemWithTitle(&NSString::from_str(&device.display()));
                }
                *devices = found;
                self.set_message("请选择设备", "从左上角设备菜单选择一个 MTP 设备。");
            }
            Err(err) => {
                popup.addItemWithTitle(ns_string!("设备扫描失败"));
                self.set_message("设备扫描失败", &format!("{err}"));
            }
        }

        self.ivars().device.borrow_mut().take();
        self.ivars().nodes.borrow_mut().clear();
        self.ivars().root_children.borrow_mut().clear();
        self.reload_outline();
    }

    fn select_current_device(&self) {
        let Some(popup) = self.ivars().device_popup.get() else {
            return;
        };
        let selected = popup.indexOfSelectedItem();
        if selected <= 0 {
            return;
        }
        let device_info = match self.ivars().devices.borrow().get((selected - 1) as usize) {
            Some(info) => info.clone(),
            None => return,
        };

        self.set_message("正在连接设备", &device_info.display());
        let result = self
            .runtime()
            .block_on(MtpDevice::open_by_location(device_info.location_id));

        match result {
            Ok(device) => {
                self.ivars().device.replace(Some(device));
                self.load_storages();
            }
            Err(err) => {
                self.ivars().device.borrow_mut().take();
                self.set_message("连接设备失败", &format_mtp_error(&err));
                self.ivars().nodes.borrow_mut().clear();
                self.ivars().root_children.borrow_mut().clear();
                self.reload_outline();
            }
        }
    }

    fn load_storages(&self) {
        let Some(device) = self.ivars().device.borrow().clone() else {
            return;
        };
        let result = self.runtime().block_on(async { device.storages().await });
        let storages = match result {
            Ok(storages) => storages,
            Err(err) => {
                self.set_message("读取存储失败", &format_mtp_error(&err));
                return;
            }
        };

        let mut nodes = Vec::new();
        let mut roots = Vec::new();
        for storage in storages {
            let info = storage.info();
            let index = nodes.len();
            roots.push(index);
            nodes.push(BrowserNode {
                name: info.description.clone(),
                kind: "存储".to_string(),
                size: format_bytes(info.free_space_bytes),
                note: format!(
                    "Storage ID: {}\n可用空间: {}",
                    storage.id().0,
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
                "设备没有可用存储",
                "MTP 设备未返回 storage 列表。",
            ));
            roots.push(0);
        }

        *self.ivars().nodes.borrow_mut() = nodes;
        *self.ivars().root_children.borrow_mut() = roots;
        self.reload_outline();
        self.update_detail();
    }

    fn load_children(&self, index: usize) {
        if let Err(message) = self.load_children_result(index, None) {
            let mut nodes = self.ivars().nodes.borrow_mut();
            let child = nodes.len();
            nodes.push(message_node("目录读取失败", &message));
            nodes[index].children = vec![child];
            nodes[index].children_loaded = true;
        }
    }

    fn load_children_result(&self, index: usize, timeout: Option<Duration>) -> Result<(), String> {
        let Some(device) = self.ivars().device.borrow().clone() else {
            return Err("设备未连接。".to_string());
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

        let result = self.runtime().block_on(async {
            let operation = async {
                let storage = device.storage(storage_id).await?;
                storage.list_objects(parent).await
            };
            match timeout {
                Some(timeout) => tokio::time::timeout(timeout, operation)
                    .await
                    .map_err(|_| format!("MTP 目录读取超过 {} 秒。", timeout.as_secs()))?
                    .map_err(|err| format_mtp_error(&err)),
                None => operation.await.map_err(|err| format_mtp_error(&err)),
            }
        });

        let objects = match result {
            Ok(objects) => objects,
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
                kind: if is_folder { "文件夹" } else { "文件" }.to_string(),
                size: if is_folder {
                    "--".to_string()
                } else {
                    format_bytes(object.size)
                },
                note: format!(
                    "Handle: {}\nStorage: {}\nQuick Look 时才会下载文件。",
                    object.handle.0, storage_id.0
                ),
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

        self.set_message("正在准备预览", "正在从 MTP 设备复制文件到临时目录。");
        let device = self.ivars().device.borrow().clone()?;
        let result = self.runtime().block_on(async {
            let storage = device.storage(storage_id).await?;
            storage.download(handle).await
        });
        let data = match result {
            Ok(data) => data,
            Err(err) => {
                self.set_message("预览失败", &format_mtp_error(&err));
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
            self.set_message("预览失败", "无法写入临时预览文件。");
            return None;
        }
        self.ivars().nodes.borrow_mut()[index].cached_path = Some(path.clone());
        self.update_detail();
        Some(path)
    }

    fn write_drag_items(&self, items: &NSArray, pasteboard: &NSPasteboard) -> bool {
        let indexes = self.node_indexes_from_items(items);
        if indexes.is_empty() {
            return false;
        }

        self.set_message("正在复制拖拽项目", "正在从 MTP 设备复制到本机临时目录。");
        let export_root = std::env::temp_dir().join("macmtp-drag").join(format!(
            "{}-{}",
            std::process::id(),
            timestamp_millis()
        ));

        let mut exported = Vec::new();
        for index in indexes {
            match self.export_node_for_drag(index, &export_root) {
                Ok(path) => exported.push(path),
                Err(message) => {
                    self.set_message("拖拽复制失败", &message);
                    return false;
                }
            }
        }

        let urls: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = exported
            .iter()
            .map(|path| {
                let ns_path = NSString::from_str(&path.to_string_lossy());
                ProtocolObject::from_retained(NSURL::fileURLWithPath(&ns_path))
            })
            .collect();

        pasteboard.clearContents();
        let objects = NSArray::from_retained_slice(&urls);
        if pasteboard.writeObjects(&objects) {
            self.set_message(
                "可以拖拽复制",
                &format!("已准备 {} 个项目，松开鼠标复制到目标位置。", exported.len()),
            );
            true
        } else {
            self.set_message("拖拽复制失败", "无法写入拖拽剪贴板。");
            false
        }
    }

    fn export_node_for_drag(&self, index: usize, parent: &PathBuf) -> Result<PathBuf, String> {
        let node = self
            .ivars()
            .nodes
            .borrow()
            .get(index)
            .cloned()
            .ok_or_else(|| "找不到要复制的项目。".to_string())?;

        match node.source {
            NodeSource::Object {
                storage_id,
                handle,
                is_folder: false,
            } => self.export_file(storage_id, handle, &node.name, parent),
            NodeSource::Object {
                is_folder: true, ..
            } => self.export_folder(index, &node.name, parent),
            _ => Err("只能拖拽 MTP 文件或文件夹。".to_string()),
        }
    }

    fn export_file(
        &self,
        storage_id: StorageId,
        handle: ObjectHandle,
        name: &str,
        parent: &PathBuf,
    ) -> Result<PathBuf, String> {
        fs::create_dir_all(parent).map_err(|err| format!("无法创建目标目录: {err}"))?;
        let path = unique_child_path(parent, &sanitize_filename(name));
        let device = self
            .ivars()
            .device
            .borrow()
            .clone()
            .ok_or_else(|| "设备未连接。".to_string())?;
        let data = self
            .runtime()
            .block_on(async {
                let operation = async {
                    let storage = device.storage(storage_id).await?;
                    storage.download(handle).await
                };
                tokio::time::timeout(DRAG_MTP_TIMEOUT, operation)
                    .await
                    .map_err(|_| format!("MTP 文件下载超过 {} 秒。", DRAG_MTP_TIMEOUT.as_secs()))?
                    .map_err(|err| format_mtp_error(&err))
            })
            .map_err(|message| message)?;

        fs::write(&path, data).map_err(|err| format!("无法写入文件: {err}"))?;
        Ok(path)
    }

    fn export_folder(&self, index: usize, name: &str, parent: &PathBuf) -> Result<PathBuf, String> {
        self.load_children_result(index, Some(DRAG_MTP_TIMEOUT))?;
        let folder = unique_child_path(parent, &sanitize_filename(name));
        fs::create_dir_all(&folder).map_err(|err| format!("无法创建文件夹: {err}"))?;

        let children = self
            .ivars()
            .nodes
            .borrow()
            .get(index)
            .map(|node| node.children.clone())
            .unwrap_or_default();
        for child in children {
            let is_copyable = self
                .ivars()
                .nodes
                .borrow()
                .get(child)
                .is_some_and(|node| node.is_file() || node.is_folder());
            if is_copyable {
                self.export_node_for_drag(child, &folder)?;
            }
        }
        Ok(folder)
    }

    fn runtime(&self) -> &Runtime {
        self.ivars().runtime.get().expect("runtime initialized")
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

fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

pub fn run() {
    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    let delegate = Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}
