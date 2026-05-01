use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadOnly, sel};
use objc2_app_kit::{
    NSApplication, NSAutoresizingMaskOptions, NSColor, NSDragOperation, NSEventModifierFlags,
    NSFont, NSMenu, NSMenuItem, NSOutlineView, NSPopUpButton, NSProgressIndicator, NSScrollView,
    NSTableColumn, NSTableViewGridLineStyle, NSTableViewStyle, NSTextField, NSView,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString, ns_string};

use crate::app::Delegate;
use crate::outline::PreviewOutlineView;

pub fn build_browser_ui(delegate: &Delegate, mtm: MainThreadMarker, content: &NSView) {
    let device_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Devices"));
    device_menu.setDelegate(Some(ProtocolObject::from_ref(delegate)));
    let device_popup = unsafe {
        NSPopUpButton::popUpButtonWithMenu_target_action(
            &device_menu,
            Some(delegate),
            Some(sel!(selectDevice:)),
        )
    };
    device_popup.addItemWithTitle(ns_string!("请选择设备"));
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
    outline.setAllowsMultipleSelection(true);
    outline.setIndentationPerLevel(16.0);
    outline.setIndentationMarkerFollowsCell(true);
    outline.setDraggingSourceOperationMask_forLocal(NSDragOperation::Copy, false);
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

    let progress = NSProgressIndicator::new(mtm);
    progress.setFrame(NSRect::new(
        NSPoint::new(666.0, 190.0),
        NSSize::new(204.0, 18.0),
    ));
    progress.setIndeterminate(false);
    progress.setMinValue(0.0);
    progress.setMaxValue(100.0);
    progress.setDoubleValue(0.0);
    progress.setHidden(true);
    progress.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin);

    content.addSubview(&scroll);
    content.addSubview(&device_popup);
    content.addSubview(&title);
    content.addSubview(&detail);
    content.addSubview(&progress);

    delegate.ivars().outline_view.set(outline).unwrap();
    delegate.ivars().device_popup.set(device_popup).unwrap();
    delegate.ivars().title_label.set(title).unwrap();
    delegate.ivars().detail_label.set(detail).unwrap();
    delegate.ivars().progress_indicator.set(progress).unwrap();
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

pub fn install_main_menu(app: &NSApplication, delegate: &Delegate, mtm: MainThreadMarker) {
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
