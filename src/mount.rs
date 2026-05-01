use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use fuser::{
    BackgroundSession, Config, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags,
    Generation, INodeNo, LockOwner, MountOption, OpenAccMode, OpenFlags, ReplyAttr, ReplyData,
    ReplyDirectory, ReplyEntry, ReplyOpen, Request,
};
use mtp_rs::mtp::{MtpDevice, MtpDeviceInfo};
use mtp_rs::ptp::ObjectInfo;
use mtp_rs::{ObjectHandle, StorageId};
use tokio::runtime::Builder;

use crate::util::{format_mtp_error, sanitize_filename};

const ROOT_INO: u64 = 1;
const STORAGE_INO_BASE: u64 = 0x4000_0000_0000_0000;
const OBJECT_INO_BASE: u64 = 0x8000_0000_0000_0000;
const TTL: Duration = Duration::from_secs(1);
const BLOCK_SIZE: u32 = 4096;

pub struct MountHandle {
    mountpoint: PathBuf,
    cache_dir: PathBuf,
    _session: BackgroundSession,
}

impl MountHandle {
    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }
}

impl Drop for MountHandle {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.cache_dir);
    }
}

pub fn macfuse_available() -> bool {
    [
        "/Library/Filesystems/macfuse.fs",
        "/Library/Frameworks/MacFUSE.framework",
        "/usr/local/lib/libfuse.dylib",
        "/opt/homebrew/lib/libfuse.dylib",
    ]
    .iter()
    .any(|path| Path::new(path).exists())
}

pub fn mount_device(
    device: MtpDevice,
    device_info: &MtpDeviceInfo,
    mtp_lock: Arc<Mutex<()>>,
) -> Result<MountHandle, String> {
    if !macfuse_available() {
        return Err("未检测到 macFUSE，跳过 Finder 挂载。".to_string());
    }

    let volume_name = volume_name(device_info);
    cleanup_existing_mountpoints(&volume_name);
    let mountpoint = unique_mountpoint(&volume_name)?;
    let cache_dir =
        std::env::temp_dir().join(format!("macmtp-fuse-cache-{}", device_info.location_id));
    fs::create_dir_all(&cache_dir).map_err(|err| format!("无法创建 FUSE 缓存目录: {err}"))?;

    let fs = MtpFuseFs::new(device, mtp_lock, cache_dir.clone());
    let options = vec![
        MountOption::RO,
        MountOption::NoDev,
        MountOption::NoSuid,
        MountOption::NoExec,
        MountOption::NoAtime,
        MountOption::FSName("MacMTP".to_string()),
        MountOption::Subtype("mtp".to_string()),
        MountOption::CUSTOM(format!("volname={volume_name}")),
        MountOption::CUSTOM("local".to_string()),
    ];
    let mut config = Config::default();
    config.mount_options = options;
    let session = fuser::spawn_mount2(fs, &mountpoint, &config)
        .map_err(|err| format!("FUSE 挂载失败: {err}"))?;

    Ok(MountHandle {
        mountpoint,
        cache_dir,
        _session: session,
    })
}

fn volume_name(device_info: &MtpDeviceInfo) -> String {
    let manufacturer = device_info.manufacturer.as_deref().unwrap_or("").trim();
    let product = device_info.product.as_deref().unwrap_or("").trim();
    let device_name = if !product.is_empty()
        && !manufacturer.is_empty()
        && product
            .to_lowercase()
            .starts_with(&manufacturer.to_lowercase())
    {
        product.to_string()
    } else if !manufacturer.is_empty() && !product.is_empty() {
        format!("{manufacturer} {product}")
    } else if !product.is_empty() {
        product.to_string()
    } else if !manufacturer.is_empty() {
        manufacturer.to_string()
    } else {
        "MTP Device".to_string()
    };

    let name = sanitize_filename(&device_name);
    format!("MacMTP - {name}")
}

fn unique_mountpoint(volume_name: &str) -> Result<PathBuf, String> {
    for candidate in mountpoint_candidates(volume_name) {
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err("无法找到可用的 /Volumes/MacMTP 挂载点名称。".to_string())
}

fn cleanup_existing_mountpoints(volume_name: &str) {
    for candidate in mountpoint_candidates(volume_name) {
        if !candidate.exists() {
            continue;
        }

        let _ = Command::new("diskutil")
            .arg("unmount")
            .arg(&candidate)
            .status();
    }

    thread::sleep(Duration::from_millis(300));

    for candidate in mountpoint_candidates(volume_name) {
        if candidate.exists() {
            let _ = fs::remove_dir(&candidate);
        }
    }
}

fn mountpoint_candidates(volume_name: &str) -> impl Iterator<Item = PathBuf> + '_ {
    (0..100).map(move |suffix| {
        if suffix == 0 {
            Path::new("/Volumes").join(volume_name)
        } else {
            Path::new("/Volumes").join(format!("{volume_name} {suffix}"))
        }
    })
}

struct MtpFuseFs {
    state: Mutex<FsState>,
    device: MtpDevice,
    mtp_lock: Arc<Mutex<()>>,
    cache_dir: PathBuf,
}

struct FsState {
    entries: HashMap<u64, FsEntry>,
    children: HashMap<u64, Vec<u64>>,
}

#[derive(Clone)]
struct FsEntry {
    ino: u64,
    parent: u64,
    name: OsString,
    kind: FsEntryKind,
    size: u64,
}

#[derive(Clone)]
enum FsEntryKind {
    Root,
    Storage {
        storage_id: StorageId,
    },
    Object {
        storage_id: StorageId,
        handle: ObjectHandle,
        is_folder: bool,
    },
}

impl MtpFuseFs {
    fn new(device: MtpDevice, mtp_lock: Arc<Mutex<()>>, cache_dir: PathBuf) -> Self {
        let mut entries = HashMap::new();
        entries.insert(
            ROOT_INO,
            FsEntry {
                ino: ROOT_INO,
                parent: ROOT_INO,
                name: OsString::from(""),
                kind: FsEntryKind::Root,
                size: 0,
            },
        );
        Self {
            state: Mutex::new(FsState {
                entries,
                children: HashMap::new(),
            }),
            device,
            mtp_lock,
            cache_dir,
        }
    }

    fn attr_for(entry: &FsEntry, uid: u32, gid: u32) -> FileAttr {
        let is_dir = matches!(
            entry.kind,
            FsEntryKind::Root
                | FsEntryKind::Storage { .. }
                | FsEntryKind::Object {
                    is_folder: true,
                    ..
                }
        );
        FileAttr {
            ino: INodeNo(entry.ino),
            size: if is_dir { 0 } else { entry.size },
            blocks: entry.size.div_ceil(512),
            atime: SystemTime::UNIX_EPOCH,
            mtime: SystemTime::UNIX_EPOCH,
            ctime: SystemTime::UNIX_EPOCH,
            crtime: SystemTime::UNIX_EPOCH,
            kind: if is_dir {
                FileType::Directory
            } else {
                FileType::RegularFile
            },
            perm: if is_dir { 0o555 } else { 0o444 },
            nlink: if is_dir { 2 } else { 1 },
            uid,
            gid,
            rdev: 0,
            blksize: BLOCK_SIZE,
            flags: 0,
        }
    }

    fn load_children(&self, parent: u64) -> Result<Vec<u64>, Errno> {
        if let Some(children) = self
            .state
            .lock()
            .map_err(|_| Errno::EIO)?
            .children
            .get(&parent)
            .cloned()
        {
            return Ok(children);
        }

        let parent_entry = self
            .state
            .lock()
            .map_err(|_| Errno::EIO)?
            .entries
            .get(&parent)
            .cloned()
            .ok_or(Errno::ENOENT)?;

        let entries = match parent_entry.kind {
            FsEntryKind::Root => self.load_storages(parent)?,
            FsEntryKind::Storage { storage_id, .. } => {
                self.load_objects(parent, storage_id, None)?
            }
            FsEntryKind::Object {
                storage_id,
                handle,
                is_folder: true,
            } => self.load_objects(parent, storage_id, Some(handle))?,
            FsEntryKind::Object { .. } => return Err(Errno::ENOTDIR),
        };

        let child_inos = entries.iter().map(|entry| entry.ino).collect::<Vec<_>>();
        let mut state = self.state.lock().map_err(|_| Errno::EIO)?;
        for entry in entries {
            state.entries.insert(entry.ino, entry);
        }
        state.children.insert(parent, child_inos.clone());
        Ok(child_inos)
    }

    fn load_storages(&self, parent: u64) -> Result<Vec<FsEntry>, Errno> {
        let storages = self.with_mtp_lock(|| {
            let runtime = Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| Errno::EIO)?;
            runtime
                .block_on(async { self.device.storages().await })
                .map_err(|err| {
                    eprintln!(
                        "MacMTP FUSE storage listing failed: {}",
                        format_mtp_error(&err)
                    );
                    Errno::EIO
                })
        })??;

        let mut names = HashMap::new();
        Ok(storages
            .into_iter()
            .enumerate()
            .map(|(index, storage)| {
                let info = storage.info();
                let name = unique_name(&mut names, sanitize_filename(&info.description));
                FsEntry {
                    ino: STORAGE_INO_BASE | index as u64,
                    parent,
                    name: OsString::from(name),
                    kind: FsEntryKind::Storage {
                        storage_id: storage.id(),
                    },
                    size: 0,
                }
            })
            .collect())
    }

    fn load_objects(
        &self,
        parent: u64,
        storage_id: StorageId,
        object_parent: Option<ObjectHandle>,
    ) -> Result<Vec<FsEntry>, Errno> {
        let objects = self.with_mtp_lock(|| {
            let runtime = Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| Errno::EIO)?;
            runtime
                .block_on(async {
                    let storage = self.device.storage(storage_id).await?;
                    storage.list_objects(object_parent).await
                })
                .map_err(|err| {
                    eprintln!(
                        "MacMTP FUSE directory listing failed: {}",
                        format_mtp_error(&err)
                    );
                    Errno::EIO
                })
        })??;

        let mut names = HashMap::new();
        Ok(objects
            .into_iter()
            .map(|object| self.object_entry(parent, storage_id, object, &mut names))
            .collect())
    }

    fn object_entry(
        &self,
        parent: u64,
        storage_id: StorageId,
        object: ObjectInfo,
        names: &mut HashMap<String, usize>,
    ) -> FsEntry {
        let name = unique_name(names, sanitize_filename(&object.filename));
        let is_folder = object.is_folder();
        FsEntry {
            ino: object_ino(storage_id, object.handle),
            parent,
            name: OsString::from(name),
            kind: FsEntryKind::Object {
                storage_id,
                handle: object.handle,
                is_folder,
            },
            size: if is_folder { 0 } else { object.size },
        }
    }

    fn entry(&self, ino: u64) -> Result<FsEntry, Errno> {
        self.state
            .lock()
            .map_err(|_| Errno::EIO)?
            .entries
            .get(&ino)
            .cloned()
            .ok_or(Errno::ENOENT)
    }

    fn cached_file(&self, entry: &FsEntry) -> Result<PathBuf, Errno> {
        let FsEntryKind::Object {
            storage_id,
            handle,
            is_folder: false,
        } = entry.kind
        else {
            return Err(Errno::EISDIR);
        };

        let path = self.cache_dir.join(format!("{}", entry.ino));
        if path.exists() {
            return Ok(path);
        }

        let tmp_path = self.cache_dir.join(format!("{}.part", entry.ino));
        self.with_mtp_lock(|| {
            let runtime = Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| Errno::EIO)?;
            runtime.block_on(async {
                let storage = self.device.storage(storage_id).await.map_err(|err| {
                    eprintln!(
                        "MacMTP FUSE storage open failed: {}",
                        format_mtp_error(&err)
                    );
                    Errno::EIO
                })?;
                let mut download = storage.download_stream(handle).await.map_err(|err| {
                    eprintln!("MacMTP FUSE download failed: {}", format_mtp_error(&err));
                    Errno::EIO
                })?;
                let mut file = File::create(&tmp_path).map_err(|_| Errno::EIO)?;
                while let Some(chunk) = download.next_chunk().await {
                    let chunk = chunk.map_err(|err| {
                        eprintln!("MacMTP FUSE read failed: {}", format_mtp_error(&err));
                        Errno::EIO
                    })?;
                    file.write_all(&chunk).map_err(|_| Errno::EIO)?;
                }
                file.flush().map_err(|_| Errno::EIO)?;
                fs::rename(&tmp_path, &path).map_err(|_| Errno::EIO)
            })
        })??;
        Ok(path)
    }

    fn with_mtp_lock<T>(&self, operation: impl FnOnce() -> T) -> Result<T, Errno> {
        let _guard = self.mtp_lock.lock().map_err(|_| Errno::EIO)?;
        Ok(operation())
    }
}

impl Filesystem for MtpFuseFs {
    fn lookup(&self, req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let result = self.load_children(parent.into()).and_then(|children| {
            let state = self.state.lock().map_err(|_| Errno::EIO)?;
            children
                .into_iter()
                .filter_map(|ino| state.entries.get(&ino))
                .find(|entry| entry.name == name)
                .map(|entry| Self::attr_for(entry, req.uid(), req.gid()))
                .ok_or(Errno::ENOENT)
        });
        match result {
            Ok(attr) => reply.entry(&TTL, &attr, Generation(0)),
            Err(err) => reply.error(err),
        }
    }

    fn getattr(&self, req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self
            .entry(ino.into())
            .map(|entry| Self::attr_for(&entry, req.uid(), req.gid()))
        {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(err) => reply.error(err),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        if flags.acc_mode() != OpenAccMode::O_RDONLY {
            reply.error(Errno::EROFS);
            return;
        }
        match self.entry(ino.into()) {
            Ok(entry)
                if matches!(
                    entry.kind,
                    FsEntryKind::Object {
                        is_folder: false,
                        ..
                    }
                ) =>
            {
                reply.opened(FileHandle(0), FopenFlags::empty());
            }
            Ok(_) => reply.error(Errno::EISDIR),
            Err(err) => reply.error(err),
        }
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let result = self.entry(ino.into()).and_then(|entry| {
            let path = self.cached_file(&entry)?;
            let file = File::open(path).map_err(|_| Errno::EIO)?;
            let mut buf = vec![0; size as usize];
            let read = file.read_at(&mut buf, offset).map_err(|_| Errno::EIO)?;
            buf.truncate(read);
            Ok(buf)
        });
        match result {
            Ok(data) => reply.data(&data),
            Err(err) => reply.error(err),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let parent_ino = ino.into();
        let result = self.load_children(parent_ino).and_then(|children| {
            let state = self.state.lock().map_err(|_| Errno::EIO)?;
            let parent = state.entries.get(&parent_ino).ok_or(Errno::ENOENT)?;
            let mut entries = vec![
                (parent_ino, FileType::Directory, OsString::from(".")),
                (parent.parent, FileType::Directory, OsString::from("..")),
            ];
            entries.extend(children.into_iter().filter_map(|child| {
                state.entries.get(&child).map(|entry| {
                    let attr = Self::attr_for(entry, 0, 0);
                    (entry.ino, attr.kind, entry.name.clone())
                })
            }));
            Ok(entries)
        });

        let Ok(entries) = result else {
            reply.error(result.err().unwrap_or(Errno::EIO));
            return;
        };
        for (i, (child_ino, kind, name)) in entries.into_iter().enumerate().skip(offset as usize) {
            if reply.add(INodeNo(child_ino), (i + 1) as u64, kind, name) {
                break;
            }
        }
        reply.ok();
    }
}

fn object_ino(storage_id: StorageId, handle: ObjectHandle) -> u64 {
    OBJECT_INO_BASE | ((storage_id.0 as u64) << 32) | handle.0 as u64
}

fn unique_name(names: &mut HashMap<String, usize>, name: String) -> String {
    let base = if name.is_empty() {
        "item".to_string()
    } else {
        name
    };
    let count = names.entry(base.clone()).or_insert(0);
    *count += 1;
    if *count == 1 {
        base
    } else {
        format!("{base} {}", *count)
    }
}
