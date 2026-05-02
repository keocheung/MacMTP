use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadOnly, sel};
use objc2_app_kit::{
    NSApplication, NSAutoresizingMaskOptions, NSButton, NSCellImagePosition, NSColor,
    NSDragOperation, NSEventModifierFlags, NSFont, NSImage, NSLineBreakMode, NSMenu, NSMenuItem,
    NSOutlineView, NSProgressIndicator, NSScrollElasticity, NSScrollView, NSSplitView,
    NSSplitViewDividerStyle, NSTableColumn, NSTableColumnResizingOptions,
    NSTableViewColumnAutoresizingStyle, NSTableViewGridLineStyle, NSTableViewStyle, NSTextField,
    NSView,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString, ns_string};

use crate::app::Delegate;
use crate::outline::PreviewOutlineView;

pub fn build_browser_ui(delegate: &Delegate, mtm: MainThreadMarker, content: &NSView) {
    let content_bounds = content.bounds();
    let split_width = content_bounds.size.width.max(720.0);
    let split_height = content_bounds.size.height.max(420.0);
    let split = NSSplitView::new(mtm);
    split.setFrame(content_bounds);
    split.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewHeightSizable | NSAutoresizingMaskOptions::ViewWidthSizable,
    );
    split.setVertical(true);
    split.setDividerStyle(NSSplitViewDividerStyle::Thin);
    split.setDelegate(Some(ProtocolObject::from_ref(delegate)));

    let sidebar = NSView::new(mtm);
    sidebar.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(240.0, split_height),
    ));
    sidebar.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewHeightSizable | NSAutoresizingMaskOptions::ViewWidthSizable,
    );

    let refresh_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("刷新"),
            Some(delegate),
            Some(sel!(refreshDevices:)),
            mtm,
        )
    };
    refresh_button.setFrame(NSRect::new(
        NSPoint::new(12.0, 516.0),
        NSSize::new(216.0, 30.0),
    ));
    refresh_button.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewMinYMargin | NSAutoresizingMaskOptions::ViewWidthSizable,
    );
    apply_button_symbol(&refresh_button, "arrow.clockwise", "刷新");

    let sidebar_title = NSTextField::labelWithString(ns_string!("设备"), mtm);
    sidebar_title.setFrame(NSRect::new(
        NSPoint::new(12.0, 486.0),
        NSSize::new(216.0, 20.0),
    ));
    sidebar_title.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
    sidebar_title.setTextColor(Some(&NSColor::secondaryLabelColor()));
    sidebar_title.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);

    let device_list = NSView::new(mtm);
    device_list.setFrame(NSRect::new(
        NSPoint::new(12.0, 12.0),
        NSSize::new(216.0, 466.0),
    ));
    device_list.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewHeightSizable | NSAutoresizingMaskOptions::ViewWidthSizable,
    );

    let outline: Retained<NSOutlineView> = PreviewOutlineView::new(mtm).into_super();
    outline.setRowHeight(24.0);
    outline.setIntercellSpacing(NSSize::new(0.0, 0.0));
    outline.setUsesAlternatingRowBackgroundColors(true);
    outline.setGridStyleMask(NSTableViewGridLineStyle::GridNone);
    outline.setStyle(NSTableViewStyle::Plain);
    outline.setColumnAutoresizingStyle(NSTableViewColumnAutoresizingStyle::NoColumnAutoresizing);
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

    let name_column = make_column(mtm, "name", "名称", 300.0);
    let kind_column = make_column(mtm, "kind", "类型", 80.0);
    let size_column = make_column(mtm, "size", "大小", 90.0);
    let created_column = make_column(mtm, "created", "添加时间", 150.0);
    let modified_column = make_column(mtm, "modified", "修改时间", 150.0);
    outline.addTableColumn(&name_column);
    outline.addTableColumn(&kind_column);
    outline.addTableColumn(&size_column);
    outline.addTableColumn(&created_column);
    outline.addTableColumn(&modified_column);
    unsafe { outline.setOutlineTableColumn(Some(&name_column)) };

    let browser_panel = NSView::new(mtm);
    browser_panel.setFrame(NSRect::new(
        NSPoint::new(240.0, 0.0),
        NSSize::new((split_width - 480.0).max(240.0), split_height),
    ));
    browser_panel.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewHeightSizable | NSAutoresizingMaskOptions::ViewWidthSizable,
    );

    let scroll = NSScrollView::new(mtm);
    scroll.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(420.0, 560.0),
    ));
    scroll.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewHeightSizable | NSAutoresizingMaskOptions::ViewWidthSizable,
    );
    scroll.setHasVerticalScroller(true);
    scroll.setHasHorizontalScroller(true);
    scroll.setHorizontalScrollElasticity(NSScrollElasticity::None);
    scroll.setVerticalScrollElasticity(NSScrollElasticity::None);
    scroll.setDocumentView(Some(&outline));

    let detail_panel = NSView::new(mtm);
    detail_panel.setFrame(NSRect::new(
        NSPoint::new((split_width - 240.0).max(480.0), 0.0),
        NSSize::new(240.0, split_height),
    ));
    detail_panel.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewHeightSizable | NSAutoresizingMaskOptions::ViewWidthSizable,
    );

    let title = NSTextField::labelWithString(ns_string!("未选择文件"), mtm);
    title.setFrame(NSRect::new(
        NSPoint::new(16.0, 430.0),
        NSSize::new(208.0, 78.0),
    ));
    title.setFont(Some(&NSFont::boldSystemFontOfSize(18.0)));
    title.setUsesSingleLineMode(false);
    title.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
    title.setMaximumNumberOfLines(3);
    title.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin,
    );

    let detail = NSTextField::labelWithString(ns_string!(""), mtm);
    detail.setFrame(NSRect::new(
        NSPoint::new(16.0, 338.0),
        NSSize::new(208.0, 76.0),
    ));
    detail.setFont(Some(&NSFont::systemFontOfSize(14.0)));
    detail.setTextColor(Some(&NSColor::secondaryLabelColor()));
    detail.setUsesSingleLineMode(false);
    detail.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
    detail.setMaximumNumberOfLines(0);
    detail.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin,
    );

    let detail_info = NSView::new(mtm);
    detail_info.setFrame(NSRect::new(
        NSPoint::new(16.0, 188.0),
        NSSize::new(208.0, 136.0),
    ));
    detail_info.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin,
    );

    let mount_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("挂载"),
            Some(delegate),
            Some(sel!(mountDevice:)),
            mtm,
        )
    };
    mount_button.setFrame(NSRect::new(
        NSPoint::new(16.0, 80.0),
        NSSize::new(208.0, 30.0),
    ));
    mount_button.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMaxYMargin,
    );
    mount_button.setTag(0);
    apply_button_symbol(&mount_button, "mount", "挂载");

    let eject_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("推出"),
            Some(delegate),
            Some(sel!(ejectDevice:)),
            mtm,
        )
    };
    eject_button.setFrame(NSRect::new(
        NSPoint::new(16.0, 44.0),
        NSSize::new(208.0, 30.0),
    ));
    eject_button.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMaxYMargin,
    );
    eject_button.setTag(0);
    apply_button_symbol(&eject_button, "eject", "推出");

    let progress = NSProgressIndicator::new(mtm);
    progress.setFrame(NSRect::new(
        NSPoint::new(16.0, 118.0),
        NSSize::new(208.0, 18.0),
    ));
    progress.setIndeterminate(false);
    progress.setMinValue(0.0);
    progress.setMaxValue(100.0);
    progress.setDoubleValue(0.0);
    progress.setHidden(true);
    progress.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin,
    );

    sidebar.addSubview(&refresh_button);
    sidebar.addSubview(&sidebar_title);
    sidebar.addSubview(&device_list);
    browser_panel.addSubview(&scroll);
    detail_panel.addSubview(&title);
    detail_panel.addSubview(&detail);
    detail_panel.addSubview(&detail_info);
    detail_panel.addSubview(&mount_button);
    detail_panel.addSubview(&eject_button);
    detail_panel.addSubview(&progress);
    split.addSubview(&sidebar);
    split.addSubview(&browser_panel);
    split.addSubview(&detail_panel);
    split.setPosition_ofDividerAtIndex(240.0, 0);
    split.setPosition_ofDividerAtIndex((split_width - 240.0).max(480.0), 1);
    split.adjustSubviews();
    content.addSubview(&split);

    delegate.ivars().outline_view.set(outline).unwrap();
    delegate.ivars().device_list_view.set(device_list).unwrap();
    delegate.ivars().refresh_button.set(refresh_button).unwrap();
    delegate
        .ivars()
        .detail_mount_button
        .set(mount_button)
        .unwrap();
    delegate
        .ivars()
        .detail_eject_button
        .set(eject_button)
        .unwrap();
    delegate.ivars().title_label.set(title).unwrap();
    delegate.ivars().detail_label.set(detail).unwrap();
    delegate.ivars().detail_info_view.set(detail_info).unwrap();
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
    column.setMinWidth(width);
    column.setResizingMask(NSTableColumnResizingOptions::UserResizingMask);
    column
}

fn apply_button_symbol(button: &NSButton, symbol_name: &str, accessibility_description: &str) {
    if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(symbol_name),
        Some(&NSString::from_str(accessibility_description)),
    ) {
        button.setImage(Some(&image));
        button.setImagePosition(NSCellImagePosition::ImageLeading);
        button.setImageHugsTitle(true);
    }
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
