mod auth;
mod item;
mod storage;
mod sync;

// There are a number of techniques used to make this code faster.
// - jemalloc seems to be faster for allocation and deallocation of the many serde objects
// - buffering peristence I/O (easy to forget that Rust files are not buffered by default)
// - adding a select clause to the retrieval link saves bandwidth and almost halves the time to sync
// A few things cause a slight speedup, but mostly save space:
// - using typed serde rather than Value to deserialize less (saves memory)
// - using CBOR for persistence rather than JSON (saves disk space)

#[global_allocator]
static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

use crate::item::{DriveSnapshot, DriveState, Item, ItemType};
use crate::sync::{sync_drive_items, DriveItemHandler};
use crate::storage::Storage;
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::time::Duration;
use reqwest::{header, StatusCode};
use serde_json::Value;
use oauth2::prelude::*;
use oauth2::basic::BasicTokenType;
use oauth2::TokenResponse;

const CRATE_NAME: Option<&str> = option_env!("CARGO_PKG_NAME");
const CRATE_VERSION: Option<&str> = option_env!("CARGO_PKG_VERSION");

// Read this before writing your own code that reveals the OAuth2 client secret!
// There are two ways to use an OAuth2 client secret to obtain unauthorized access:
//
// 1. Use the client credentials flow - simply login in with the client id and secret to obtain an
// access token. To prevent this being a problem, ensure that the client credentials give no access.
// In the Microsoft Graph interface, Application Permissions provide the allowed scopes for client
// credential login. Do not allow any Application Permissions! Use Delegated permissions only.
//
// 2. Intercept an authorization code redirect, and then use the authorization code and the client
// secret to obtain an access token. This code uses PKCE to prevent this. Any request using the
// authorization code and client secret must also provide the PKCE code verifier, which is kept
// secret. The client secret doesn't add any security when using PKCE, but Microsoft Graph does not
// support the authorization code flow without it.
//
const CLIENT_ID: &str = "6612d641-e7d8-4d39-8dac-e6f21efe1bf4";
const CLIENT_SECRET: &str = "ubnDYPYV4019]pentXO1~[=";

fn cache_filename(project: &directories::ProjectDirs, drive_id: &str) -> std::path::PathBuf {
    let mut cache_path = project.cache_dir().to_path_buf();
    if let Err(_) = std::fs::create_dir_all(&cache_path) {
        // let a later error sort it out
    }
    // Increment the number after `drive` when the serialized format changes
    cache_path.push(format!("drive1_{}", drive_id));
    cache_path.set_extension("cbor");
    cache_path
}

#[derive(PartialEq, Eq, Hash)]
enum ItemHash {
    Sha1(String),
    QuickXor(String),
}

struct ItemHandler<'a> {
    state: &'a mut DriveState,
    bar: &'a indicatif::ProgressBar,
}

impl<'a> DriveItemHandler<Item> for ItemHandler<'a> {
    fn reset(&mut self) {
        let size = self.state.reset();
        self.bar.set_position(size);
    }

    fn handle(&mut self, item: Item) {
        let size = if item.deleted.is_some() {
            self.state.delete(item)
        }
        else {
            self.state.upsert(item)
        };
        self.bar.set_position(size);
    }
}

fn sync_items(client: &reqwest::Client, mut snapshot: DriveSnapshot, bar: &indicatif::ProgressBar) -> Result<DriveSnapshot, Box<dyn Error>> {
    let mut handler = ItemHandler {
        state: &mut snapshot.state,
        bar,
    };
    snapshot.delta_link = sync_drive_items(client, snapshot.delta_link, &mut handler)?;
    Ok(snapshot)
}

fn ignore_file(mime_type: &Option<String>) -> bool {
    // Files with the "application/msonenote" MIME Type do not have a SHA
    mime_type.as_ref().map_or(false, |s| s == "application/msonenote")
}

fn ignore_path(dirname: &str, basename: &str) -> bool {
    // SVN repo files may be duplicated in the .svn directory. Don't match these,
    // as they are part of the SVN repo format, and should not be modified
    // individually.
    basename.ends_with(".svn-base") && dirname.contains("/.svn/pristine/")
}

fn analyze_items(names_by_hash: &HashMap<String, Item>)
    -> (u32, u32, BTreeMap<u64, HashMap<ItemHash, Vec<String>>>)
{
    let mut names_by_hash_by_size = BTreeMap::<u64, HashMap<ItemHash, Vec<String>>>::new();
    let mut file_count = 0;
    let mut folder_count = 0;
    let bar = indicatif::ProgressBar::new(names_by_hash.len() as u64);
    bar.set_style(indicatif::ProgressStyle::default_bar()
        .template("Analyzing duplicates: [{elapsed_precise}] {wide_bar} {percent}%")
        .progress_chars("#>-"));
    bar.tick();
    for item in names_by_hash.values() {
        bar.inc(1);
        match &item.item_type {
            ItemType::File { mime_type, hashes } => {
                file_count += 1;
                if ignore_file(&mime_type) {
                    continue;
                }
                let dirname = item.parent.path.trim_start_matches("/drive/root:/");
                if ignore_path(dirname, &item.name) {
                    continue;
                }
                let hash = match hashes {
                    Some(hashes) => {
                        match item.parent.drive_type.as_ref() {
                            "personal" => {
                                match hashes.sha {
                                    Some(ref sha) => {
                                        ItemHash::Sha1(sha.clone())
                                    }
                                    None => {
                                        eprintln!("Ignoring item due to missing sha1 hash: {:?}\n", item);
                                        continue;
                                    }
                                }
                            },
                            "business" | "documentLibrary" => {
                                match hashes.xor {
                                    Some(ref xor) => {
                                        ItemHash::QuickXor(xor.clone())
                                    }
                                    None => {
                                        eprintln!("Ignoring item due to missing quickXor hash: {:?}\n", item);
                                        continue;
                                    }
                                }
                            },
                            _ => {
                                eprintln!("Ignoring item due to unknown drive_type: {:?}\n", item);
                                continue;
                            }
                        }
                    }
                    None => {
                        eprintln!("Ignoring item due to missing hashes: {:?}\n", hashes);
                        continue;
                    }
                };
                let names_by_hash = names_by_hash_by_size.entry(item.size).or_insert_with(HashMap::<ItemHash, Vec<String>>::new);
                // allocating the key only on insert is messy - we could use raw_entry here,
                // or maybe entry_ref() will exist one day - for now, always allocate
                let v = names_by_hash.entry(hash).or_insert_with(Vec::<String>::new);
                let name = format!("{}/{}", dirname, item.name);
                v.push(name);
            }
            ItemType::Folder {} | ItemType::Package {} => {
                folder_count += 1;
            }
        }
    }
    bar.finish_and_clear();
    (file_count, folder_count, names_by_hash_by_size)
}

fn main() {
    let project_dirs = directories::ProjectDirs::from("Casa", "Giddy", "MSOD-stat");
    let token = auth::authenticate(CLIENT_ID.to_owned(), CLIENT_SECRET.to_owned()).unwrap();
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::USER_AGENT,
        header::HeaderValue::from_str(
            &format!(
                "{}/{}",
                CRATE_NAME.unwrap_or("msod-stat"),
                CRATE_VERSION.unwrap_or("unknown"),
            )
        ).unwrap());
    match token.token_type() {
        BasicTokenType::Bearer => {
            headers.insert(
                header::AUTHORIZATION,
                header::HeaderValue::from_str(
                    &format!("Bearer {}", token.access_token().secret().to_string())
                ).unwrap()
            );
        },
        BasicTokenType::Mac => {
            panic!("reqwest does not support MAC Authorization")
        }
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .default_headers(headers)
        .build().unwrap();

    let mut response = client.get("https://graph.microsoft.com/v1.0/me/drives").send().unwrap();
    if response.status() != StatusCode::OK {
        panic!("{:?} {}", response.status(), response.status().canonical_reason().unwrap());
    }
    let result = response.text().unwrap();
    let json: Value = serde_json::from_str(&result).unwrap();
    for drive in json["value"].as_array().unwrap() {
        let drive_id = drive["id"].as_str().unwrap();
        let quota = &drive["quota"];
        let total = quota["total"].as_u64().unwrap();
        let used = quota["used"].as_u64().unwrap();
        let deleted = quota["deleted"].as_u64().unwrap();
        let remaining = quota["remaining"].as_u64().unwrap();
        assert!(used + remaining == total);
        println!();
        println!("Drive {}", drive_id);
        println!("total:  {:>18}", size_as_string(total));
        println!("free:   {:>18}", size_as_string(remaining));
        println!(
            "used:   {:>18} = {:.2}% (including {} pending deletion)",
            size_as_string(used),
            used as f32 * 100.0 / total as f32,
            size_as_string(deleted)
        );
        let bar = indicatif::ProgressBar::new(used);
        bar.set_style(indicatif::ProgressStyle::default_bar()
            .template("Fetching drive data: [{elapsed_precise}] {wide_bar} {percent}%")
            .progress_chars("#>-"));
        bar.enable_steady_tick(100);
        let cache = Storage::new(project_dirs.as_ref().map(|dir| cache_filename(dir, drive_id)));
        let snapshot = cache.load().unwrap_or_else(|| DriveSnapshot::default(drive_id));
        bar.set_position(snapshot.state.size);
        let snapshot = sync_items(&client, snapshot, &bar).unwrap();
        cache.save(&snapshot);
        bar.finish_and_clear();
        let (file_count, folder_count, names_by_hash_by_size) = analyze_items(&snapshot.state.items);
        println!("folders:{:>10}", folder_count);
        println!("files:  {:>10}", file_count);
        println!("duplicates:");
        for (size, names_by_hash) in names_by_hash_by_size.iter().rev() {
            for names in names_by_hash.values() {
                if names.len() > 1 {
                    println!("{}", size_as_string(*size));
                    for name in names {
                        println!("\t{}", name);
                    }
                }
            }
        }
    }
}

fn size_as_string(value: u64) -> String {
    if value < 32 * 1024 {
        format!("{} bytes", value)
    }
    else {
        let mib = value as f32 / 1024.0 / 1024.0;
        if mib < 1000.0 {
            format!("{:.3} MiB", mib)
        }
        else {
            let gib = mib / 1024.0;
            format!("{:.3} GiB", gib)
        }
    }

}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use super::{Item, ItemType};

    #[test]
    fn json_file() {
        let data = json!({
            "id": "ID",
            "name": "NAME",
            "size": 8192,
            "file": {
                "hashes": {
                    "quickXorHash": "ZBIxs/4bmb5QuzTKkGJbU+7IsfM=",
                    "sha1Hash": "9784E164A3626978D838EE21A0319C0DFB39001B"
                },
                "mimeType": "image/jpeg"
            },
        }).to_string();
        let item: Item = serde_json::from_str(&data).unwrap();
        assert_eq!(item.id, "ID");
        assert_eq!(item.name, "NAME");
        assert_eq!(item.size, 8192);
        match item.item_type {
            ItemType::File{ mime_type, .. } => {
                assert_eq!(mime_type, Some("image/jpeg".to_owned()));
            }
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
            "package": {
                "view": {
                    "sortBy": "takenOrCreatedDateTime",
                    "sortOrder": "descending",
                    "viewType": "thumbnails"
                }
            }
        }).to_string();
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
            "folder": {
                "view": {
                    "sortBy": "takenOrCreatedDateTime",
                    "sortOrder": "descending",
                    "viewType": "thumbnails"
                }
            }
        }).to_string();
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
            "file": {
                "hashes": {
                    "quickXorHash": "ZBIxs/4bmb5QuzTKkGJbU+7IsfM=",
                    "sha1Hash": "9784E164A3626978D838EE21A0319C0DFB39001B"
                },
                "mimeType": "image/jpeg"
            },
            "deleted": {}
        }).to_string();
        let item: Item = serde_json::from_str(&data).unwrap();
        assert_eq!(item.id, "ID");
        assert_eq!(item.name, "NAME");
        assert_eq!(item.size, 8192);
        match item.item_type {
            ItemType::File{ mime_type, .. } => {
                assert_eq!(mime_type, Some("image/jpeg".to_owned()));
            }
            _ => {
                panic!("Not a file!");
            }
        }
        assert!(item.deleted.is_some());
    }
}
