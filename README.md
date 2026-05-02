# MacMTP

[简体中文 README](docs/README.zh-Hans.md)

MacMTP is a lightweight native macOS MTP device file browser for browsing and exporting files from Android phones, Kindle devices, Nintendo Switch (2), and other MTP devices.

## Features

- Scan and connect to MTP devices
- Manually mount devices into macOS when [macFUSE](https://macfuse.github.io/) is installed
- Browse device storage and files in a tree view
- Drag files to Finder to copy them to your Mac
- Press Space for Quick Look previews
- Native English and Simplified Chinese localization

## Requirements

- macOS 10.15+
- macFUSE (optional; enables mounting)
- Rust toolchain (edition 2024)

## Build and Run

```bash
cargo build --release
cargo run
```

## Packaging

```bash
cargo packager
```

The package configuration includes English and Simplified Chinese `.lproj` resources so macOS can select the app language from System Settings.

## Usage

1. Connect an Android phone or other MTP device to your Mac.
2. Launch MacMTP and select a device from the left device list.
3. If macFUSE is installed, click the mount button next to a device. The device appears under `/Volumes/MacMTP - ...` and can be browsed in Finder.
4. Browse device files, then drag selected files to Finder to copy them.
5. Select a file and press Space to preview it with Quick Look.

## Tech Stack

- Rust + tokio async runtime
- mtp-rs for MTP communication
- fuser + macFUSE for read-only Finder mounts
- objc2 for native macOS integration
