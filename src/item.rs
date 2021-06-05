use serde_derive::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize)]
pub struct Exists {
    // empty struct to avoid deserializing contents of JSON object
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Hash {
    #[serde(rename = "sha1Hash", default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(
        rename = "quickXorHash",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub xor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Parent {
    // Deleted parent may have no path
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(rename = "driveType")]
    pub drive_type: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum ItemType {
    #[serde(rename = "file")]
    File {
        // OneNote files do not have hashes
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hashes: Option<Hash>,
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
    #[serde(default)] // a deleted item has no size, use 0
    pub size: u64,
    #[serde(rename = "parentReference")]
    pub parent: Parent,
    #[serde(flatten)] // item_type replaced in serialization with one of file, folder, package
    pub item_type: ItemType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted: Option<Exists>,
}

#[derive(Serialize, Deserialize)]
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
        if let ItemType::File { .. } = item.item_type {
            self.size += item.size;
        }
        if let Some(prev) = self.items.insert(item.id.clone(), item) {
            if let ItemType::File { .. } = prev.item_type {
                let size = prev.size;
                assert!(size <= self.size);
                self.size -= size;
            };
        };
        self.size
    }

    pub fn delete(&mut self, item: Item) -> u64 {
        if let Some(prev) = self.items.remove(&item.id) {
            if let ItemType::File { .. } = prev.item_type {
                let size = prev.size;
                assert!(size <= self.size);
                self.size -= size;
            }
        }
        self.size
    }
}

#[derive(Serialize, Deserialize)]
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
                items: HashMap::new(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Item, ItemType};
    use serde_json::json;

    #[test]
    fn json_file() {
        let data = json!({
            "id": "ID",
            "name": "NAME",
            "size": 8192,
            "parentReference": {
                "path": "NAME",
                "driveType": "personal"
            },
            "file": {
                "hashes": {
                    "quickXorHash": "ZBIxs/4bmb5QuzTKkGJbU+7IsfM=",
                    "sha1Hash": "9784E164A3626978D838EE21A0319C0DFB39001B"
                },
            },
        })
        .to_string();
        let item: Item = serde_json::from_str(&data).unwrap();
        assert_eq!(item.id, "ID");
        assert_eq!(item.name, "NAME");
        assert_eq!(item.size, 8192);
        match item.item_type {
            ItemType::File { .. } => {}
            _ => {
                panic!("Not a file!");
            }
        }
        assert!(item.deleted.is_none());
    }

    #[test]
    fn json_package() {
        let data = json!({
            "id": "ID",
            "name": "NAME",
            "size": 8192,
            "parentReference": {
                "path": "NAME",
                "driveType": "personal"
            },
            "package": {
                "view": {
                    "sortBy": "takenOrCreatedDateTime",
                    "sortOrder": "descending",
                    "viewType": "thumbnails"
                }
            }
        })
        .to_string();
        let item: Item = serde_json::from_str(&data).unwrap();
        assert_eq!(item.id, "ID");
        assert_eq!(item.name, "NAME");
        assert_eq!(item.size, 8192);
        assert_eq!(item.item_type, ItemType::Package {});
        assert!(item.deleted.is_none());
    }

    #[test]
    fn json_folder() {
        let data = json!({
            "id": "ID",
            "name": "NAME",
            "size": 8192,
            "parentReference": {
                "path": "NAME",
                "driveType": "personal"
            },
            "folder": {
                "view": {
                    "sortBy": "takenOrCreatedDateTime",
                    "sortOrder": "descending",
                    "viewType": "thumbnails"
                }
            }
        })
        .to_string();
        let item: Item = serde_json::from_str(&data).unwrap();
        assert_eq!(item.id, "ID");
        assert_eq!(item.name, "NAME");
        assert_eq!(item.size, 8192);
        assert_eq!(item.item_type, ItemType::Folder {});
        assert!(item.deleted.is_none());
    }

    #[test]
    fn json_deleted() {
        let data = json!({
            "id": "ID",
            "name": "NAME",
            "size": 8192,
            "parentReference": {
                // deleting both the file and its parent gives a deleted file entry with no parent path
                "driveType": "personal"
            },
            "file": {
                "hashes": {
                    "quickXorHash": "ZBIxs/4bmb5QuzTKkGJbU+7IsfM=",
                    "sha1Hash": "9784E164A3626978D838EE21A0319C0DFB39001B"
                },
            },
            "deleted": {}
        })
        .to_string();
        let item: Item = serde_json::from_str(&data).unwrap();
        assert_eq!(item.id, "ID");
        assert_eq!(item.name, "NAME");
        assert_eq!(item.size, 8192);
        match item.item_type {
            ItemType::File { .. } => {}
            _ => {
                panic!("Not a file!");
            }
        }
        assert!(item.deleted.is_some());
    }
}
