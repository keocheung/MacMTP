# MacMTP

macOS 的轻量级 MTP 设备文件浏览器，支持从 Android 手机、Kindle、Nintendo Switch (2) 等 MTP 设备浏览和导出文件。

## 功能特性

- 扫描和连接 MTP 设备
- 检测到 [macFUSE](https://macfuse.github.io/) 时可手动挂载到 macOS
- 树形目录浏览设备存储和文件
- 拖拽文件到 Finder 复制到本机
- 空格键 Quick Look 预览文件
- 原生英文和简体中文多语言支持

## 要求

- macOS 10.15+
- macFUSE（可选；安装后启用挂载）
- Rust 工具链 (edition 2024)

## 构建与运行

```bash
cargo build --release
cargo run
```

## 打包

```bash
cargo packager
```

打包配置会把英文和简体中文 `.lproj` 资源放入 app bundle，macOS 可在系统设置中为应用选择语言。

## 使用方法

1. 连接 Android 手机或 MTP 设备到 Mac
2. 启动应用，从左侧设备栏选择设备
3. 如果已安装 macFUSE，点击设备行旁边的挂载按钮后，设备会出现在 `/Volumes/MacMTP - ...` 并可在 Finder 浏览
4. 浏览设备文件，选中文件可拖拽到 Finder 复制
5. 选中文件按空格键可预览内容

## 技术栈

- Rust + tokio 异步运行时
- mtp-rs 实现 MTP 协议通信
- fuser + macFUSE 实现只读 Finder 挂载
- objc2 实现与 macOS 的集成
