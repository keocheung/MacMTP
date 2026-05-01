use std::cell::{OnceCell, RefCell};
use std::fs;
use std::path::PathBuf;

use mtp_rs::mtp::{MtpDevice, MtpDeviceInfo};
use mtp_rs::{ObjectHandle, StorageId};
use objc2_quartz as _;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSAutoresizingMaskOptions,
    NSBackingStoreType, NSColor, NSControlTextEditingDelegate, NSEvent, NSEventModifierFlags,
    NSFont, NSMenu, NSMenuItem, NSOutlineView, NSOutlineViewDataSource, NSOutlineViewDelegate,
    NSPopUpButton, NSScrollView, NSTableColumn, NSTableViewGridLineStyle, NSTableViewStyle,
    NSTextField, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSInteger, NSNotification, NSNumber, NSObject, NSObjectProtocol, NSPoint,
    NSRect, NSSize, NSString, NSURL, ns_string,
};
use tokio::runtime::{Builder, Runtime};

#[derive(Clone, Debug)]
struct BrowserNode {
    name: String,
    kind: String,
    size: String,
    note: String,
    source: NodeSource,
    children: Vec<usize>,
    children_loaded: bool,
    can_expand: bool,
    cached_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
enum NodeSource {
    Message,
    Storage {
        storage_id: StorageId,
    },
    Object {
        storage_id: StorageId,
        handle: ObjectHandle,
        is_folder: bool,
    },
}

#[derive(Default)]
struct AppDelegateIvars {
    window: OnceCell<Retained<NSWindow>>,
    outline_view: OnceCell<Retained<NSOutlineView>>,
    device_popup: OnceCell<Retained<NSPopUpButton>>,
    title_label: OnceCell<Retained<NSTextField>>,
    detail_label: OnceCell<Retained<NSTextField>>,
    runtime: OnceCell<Runtime>,
    devices: RefCell<Vec<MtpDeviceInfo>>,
    device: RefCell<Option<MtpDevice>>,
    nodes: RefCell<Vec<BrowserNode>>,
    root_children: RefCell<Vec<usize>>,
}

define_class!(
    #[unsafe(super = NSOutlineView)]
    #[thread_kind = MainThreadOnly]
    struct PreviewOutlineView;

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
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppDelegateIvars]
    struct Delegate;

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
        let Some(device) = self.ivars().device.borrow().clone() else {
            return;
        };
        if self
            .ivars()
            .nodes
            .borrow()
            .get(index)
            .is_none_or(|node| node.children_loaded)
        {
            return;
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
                _ => return,
            }
        };

        let result = self.runtime().block_on(async {
            let storage = device.storage(storage_id).await?;
            storage.list_objects(parent).await
        });

        let objects = match result {
            Ok(objects) => objects,
            Err(err) => {
                let child = {
                    let mut nodes = self.ivars().nodes.borrow_mut();
                    let child = nodes.len();
                    nodes.push(message_node("目录读取失败", &format_mtp_error(&err)));
                    nodes[index].children = vec![child];
                    nodes[index].children_loaded = true;
                    child
                };
                let _ = child;
                return;
            }
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

fn build_browser_ui(delegate: &Delegate, mtm: MainThreadMarker, content: &NSView) {
    let device_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Devices"));
    let device_popup = unsafe {
        NSPopUpButton::popUpButtonWithMenu_target_action(
            &device_menu,
            Some(delegate),
            Some(sel!(selectDevice:)),
        )
    };
    device_popup.setFrame(NSRect::new(
        NSPoint::new(12.0, 526.0),
        NSSize::new(360.0, 26.0),
    ));
    device_popup.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMaxYMargin);

    let outline: Retained<NSOutlineView> = PreviewOutlineView::new(mtm).into_super();
    outline.setRowHeight(24.0);
    outline.setIntercellSpacing(NSSize::new(0.0, 0.0));
    outline.setUsesAlternatingRowBackgroundColors(true);
    outline.setGridStyleMask(NSTableViewGridLineStyle::GridNone);
    outline.setStyle(NSTableViewStyle::Plain);
    outline.setAllowsMultipleSelection(false);
    outline.setIndentationPerLevel(16.0);
    outline.setIndentationMarkerFollowsCell(true);
    unsafe {
        outline.setDataSource(Some(ProtocolObject::from_ref(delegate)));
        outline.setDelegate(Some(ProtocolObject::from_ref(delegate)));
        outline.setTarget(Some(delegate));
        outline.setDoubleAction(Some(sel!(showQuickLook:)));
    }

    let name_column = make_column(mtm, "name", "名称", 360.0);
    let kind_column = make_column(mtm, "kind", "类型", 150.0);
    let size_column = make_column(mtm, "size", "大小", 110.0);
    outline.addTableColumn(&name_column);
    outline.addTableColumn(&kind_column);
    outline.addTableColumn(&size_column);
    unsafe { outline.setOutlineTableColumn(Some(&name_column)) };

    let scroll = NSScrollView::new(mtm);
    scroll.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(640.0, 520.0),
    ));
    scroll.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewHeightSizable | NSAutoresizingMaskOptions::ViewWidthSizable,
    );
    scroll.setHasVerticalScroller(true);
    scroll.setDocumentView(Some(&outline));

    let title = NSTextField::labelWithString(ns_string!("未选择文件"), mtm);
    title.setFrame(NSRect::new(
        NSPoint::new(664.0, 468.0),
        NSSize::new(212.0, 40.0),
    ));
    title.setFont(Some(&NSFont::boldSystemFontOfSize(18.0)));
    title.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin);

    let detail = NSTextField::labelWithString(ns_string!(""), mtm);
    detail.setFrame(NSRect::new(
        NSPoint::new(666.0, 228.0),
        NSSize::new(204.0, 210.0),
    ));
    detail.setFont(Some(&NSFont::systemFontOfSize(14.0)));
    detail.setTextColor(Some(&NSColor::secondaryLabelColor()));
    detail.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin);

    content.addSubview(&scroll);
    content.addSubview(&device_popup);
    content.addSubview(&title);
    content.addSubview(&detail);

    delegate.ivars().outline_view.set(outline).unwrap();
    delegate.ivars().device_popup.set(device_popup).unwrap();
    delegate.ivars().title_label.set(title).unwrap();
    delegate.ivars().detail_label.set(detail).unwrap();
}

fn make_column(
    mtm: MainThreadMarker,
    identifier: &str,
    title: &str,
    width: f64,
) -> Retained<NSTableColumn> {
    let column = NSTableColumn::initWithIdentifier(
        NSTableColumn::alloc(mtm),
        &NSString::from_str(identifier),
    );
    column.setTitle(&NSString::from_str(title));
    column.setWidth(width);
    column
}

fn install_main_menu(app: &NSApplication, delegate: &Delegate, mtm: MainThreadMarker) {
    let main_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Main"));
    let app_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!("MacMTP"),
            None,
            ns_string!(""),
        )
    };
    main_menu.addItem(&app_item);

    let app_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("MacMTP"));
    let quit_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!("退出 MacMTP"),
            Some(sel!(terminate:)),
            ns_string!("q"),
        )
    };
    app_menu.addItem(&quit_item);
    app_item.setSubmenu(Some(&app_menu));

    let file_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!("File"),
            None,
            ns_string!(""),
        )
    };
    main_menu.addItem(&file_item);

    let file_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("File"));
    let quicklook_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!("Quick Look"),
            Some(sel!(showQuickLook:)),
            ns_string!(" "),
        )
    };
    quicklook_item.setKeyEquivalentModifierMask(NSEventModifierFlags::empty());
    unsafe { quicklook_item.setTarget(Some(delegate)) };
    file_menu.addItem(&quicklook_item);
    file_item.setSubmenu(Some(&file_menu));

    let device_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!("Device"),
            None,
            ns_string!(""),
        )
    };
    main_menu.addItem(&device_item);

    let device_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Device"));
    let refresh_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!("Refresh Devices"),
            Some(sel!(refreshDevices:)),
            ns_string!("r"),
        )
    };
    refresh_item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
    unsafe { refresh_item.setTarget(Some(delegate)) };
    device_menu.addItem(&refresh_item);
    device_item.setSubmenu(Some(&device_menu));

    app.setMainMenu(Some(&main_menu));
}

impl BrowserNode {
    fn is_file(&self) -> bool {
        matches!(
            self.source,
            NodeSource::Object {
                is_folder: false,
                ..
            }
        )
    }
}

fn message_node(title: &str, detail: &str) -> BrowserNode {
    BrowserNode {
        name: title.to_string(),
        kind: "状态".to_string(),
        size: "--".to_string(),
        note: detail.to_string(),
        source: NodeSource::Message,
        children: Vec::new(),
        children_loaded: true,
        can_expand: false,
        cached_path: None,
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '\0' => '_',
            _ => ch,
        })
        .collect();
    if cleaned.is_empty() {
        "preview.bin".to_string()
    } else {
        cleaned
    }
}

fn format_mtp_error(err: &mtp_rs::Error) -> String {
    let message = err.to_string();
    if err.is_exclusive_access() {
        format!(
            "{message}\n\nmacOS 的 ptpcamerad 或 Android File Transfer 可能占用了设备。请退出相关程序，必要时临时运行: pkill -9 ptpcamerad"
        )
    } else {
        message
    }
}

fn main() {
    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    let delegate = Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}
