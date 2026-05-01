# MacMTP

macOS 的 MTP 设备文件浏览器，支持从 Android 手机、Kindle、Nintendo Switch (2) 等 MTP 设备浏览和导出文件。

## 功能特性

- 扫描和连接 MTP 设备
- 树形目录浏览设备存储和文件
- 拖拽文件到 Finder 复制到本机
- 空格键 Quick Look 预览文件
- 显示文件大小、存储信息等详情

## 要求

- macOS 10.15+
- Rust 工具链 (edition 2024)

## 构建与运行

```bash
cargo build --release
cargo run
```

## 使用方法

1. 连接 Android 手机或 MTP 设备到 Mac
2. 启动应用，从菜单选择设备
3. 浏览设备文件，选中文件可拖拽到 Finder 复制
4. 选中文件按空格键可预览内容

## 技术栈

- Rust + tokio 异步运行时
- mtp-rs 实现 MTP 协议通信
- objc2 实现与 macOS 的集成
