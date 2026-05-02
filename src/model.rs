use std::path::PathBuf;

use mtp_rs::ptp::DateTime;
use mtp_rs::{ObjectHandle, StorageId};

use crate::loc::tr;

#[derive(Clone, Debug)]
pub struct BrowserNode {
    pub name: String,
    pub kind: String,
    pub size: String,
    pub created: Option<DateTime>,
    pub modified: Option<DateTime>,
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
        kind: tr("Status"),
        size: "--".to_string(),
        created: None,
        modified: None,
        note: detail.to_string(),
        source: NodeSource::Message,
        children: Vec::new(),
        children_loaded: true,
        can_expand: false,
        cached_path: None,
    }
}
