use std::path::PathBuf;

use mtp_rs::{ObjectHandle, StorageId};

#[derive(Clone, Debug)]
pub struct BrowserNode {
    pub name: String,
    pub kind: String,
    pub size: String,
    pub note: String,
    pub source: NodeSource,
    pub children: Vec<usize>,
    pub children_loaded: bool,
    pub can_expand: bool,
    pub cached_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub enum NodeSource {
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

impl BrowserNode {
    pub fn is_file(&self) -> bool {
        matches!(
            self.source,
            NodeSource::Object {
                is_folder: false,
                ..
            }
        )
    }

    pub fn is_folder(&self) -> bool {
        matches!(
            self.source,
            NodeSource::Object {
                is_folder: true,
                ..
            }
        )
    }
}

pub fn message_node(title: &str, detail: &str) -> BrowserNode {
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
