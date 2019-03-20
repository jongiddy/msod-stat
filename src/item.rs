use std::collections::HashMap;
use serde_derive::{Serialize,Deserialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Exists {
    // empty struct to avoid deserializing contents of JSON object
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Hash {
    #[serde(rename = "sha1Hash")]
    pub sha: String
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Parent {
    pub path: String
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum ItemType {
    #[serde(rename = "file")]
    File {
        // Deleted files have an empty "file" object so all fields must be optional.
        #[serde(rename = "mimeType", default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        // OneNote files do not have hashes
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hashes: Option<Hash>
    },
    #[serde(rename = "folder")]
    Folder {},
    #[serde(rename = "package")]
    Package {},
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub name: String,
    #[serde(default)]  // a deleted item has no size, use 0
    pub size: u64,
    #[serde(rename = "parentReference", default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<Parent>,
    #[serde(flatten)]  // item_type replaced in serialization with one of file, folder, package
    pub item_type: ItemType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted: Option<Exists>,
}

#[derive(Serialize,Deserialize)]
pub struct DriveState {
    pub size: u64,
    pub items: HashMap<String, Item>,
}

impl DriveState {
    pub fn reset(&mut self) -> u64 {
        self.size = 0;
        self.items.clear();
        self.size
    }

    pub fn upsert(&mut self, item: Item) -> u64 {
        if let ItemType::File {..} = item.item_type {
            self.size += item.size;
        }
        if let Some(prev) = self.items.insert(item.id.clone(), item) {
            if let ItemType::File {..} = prev.item_type {
                let size = prev.size;
                assert!(size <= self.size);
                self.size -= size;
            };
        };
        self.size
    }

    pub fn delete(&mut self, item: Item) -> u64 {
        if let Some(prev) = self.items.remove(&item.id) {
            if let ItemType::File {..} = prev.item_type {
                let size = prev.size;
                assert!(size <= self.size);
                self.size -= size;
            }
        }
        self.size
    }
}

#[derive(Serialize,Deserialize)]
pub struct DriveSnapshot {
    pub delta_link: String,
    #[serde(flatten)]
    pub state: DriveState,
}

impl DriveSnapshot {
    pub fn default(drive_id: &str) -> DriveSnapshot {
        // an initial state that will scan entire drive
        const PREFIX: &str = "https://graph.microsoft.com/v1.0/me/drives/";
        const SUFFIX: &str = concat!(
            "/root/delta",
            "?select=id,name,size,parentReference,file,folder,package,deleted"
        );
        let mut link = String::with_capacity(PREFIX.len() + drive_id.len() + SUFFIX.len());
        link.push_str(PREFIX);
        link.push_str(drive_id);
        link.push_str(SUFFIX);
        DriveSnapshot {
            delta_link: link,
            state: DriveState {
                size: 0,
                items: HashMap::new()
            }
        }
    }
}
