mod auth;
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

use crate::sync::{sync_drive_items, DriveItemHandler};
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::time::Duration;
use rand::{thread_rng, Rng};
use reqwest::{header, StatusCode};
use serde_derive::{Serialize,Deserialize};
use serde_json::Value;
use oauth2::prelude::*;
use oauth2::basic::BasicTokenType;
use oauth2::TokenResponse;

const CRATE_NAME: Option<&str> = option_env!("CARGO_PKG_NAME");
const CRATE_VERSION: Option<&str> = option_env!("CARGO_PKG_VERSION");
const REQWEST_VERSION: &str = "0.9.11";

// Making the OAuth2 client secret public is secure because PKCE ensures
// that only the originator can use the authorization code.
const CLIENT_ID: &str = "6612d641-e7d8-4d39-8dac-e6f21efe1bf4";
const CLIENT_SECRET: &str = "ubnDYPYV4019]pentXO1~[=";

#[derive(Debug, Serialize, Deserialize)]
struct Exists {
    // empty struct to avoid deserializing contents of JSON object
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct Hash {
    #[serde(rename = "sha1Hash")]
    sha: String
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct Parent {
    path: String
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
enum ItemType {
    #[serde(rename = "file")]
    File {
        // Never seen a file without a mimeType, but the existence of the `processingMetadata`
        // attribute at https://docs.microsoft.com/en-us/onedrive/developer/rest-api/resources/file
        // suggests that it might happen
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
struct Item {
    id: String,
    name: String,
    #[serde(default)]  // a deleted item has no size, use 0
    size: u64,
    #[serde(rename = "parentReference", default, skip_serializing_if = "Option::is_none")]
    parent: Option<Parent>,
    #[serde(flatten)]  // item_type replaced in serialization with one of file, folder, package
    item_type: ItemType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deleted: Option<Exists>,
}

#[derive(Serialize,Deserialize)]
struct DriveState {
    size: u64,
    items: HashMap<String, Item>,
}

impl DriveState {
    fn reset(&mut self) -> u64 {
        self.size = 0;
        self.items.clear();
        self.size
    }

    fn upsert(&mut self, item: Item) -> u64 {
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

    fn delete(&mut self, item: Item) -> u64 {
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
struct DriveSnapshot {
    delta_link: String,
    #[serde(flatten)]
    state: DriveState,
}

impl DriveSnapshot {
    fn default(drive_id: &str) -> DriveSnapshot {
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

fn cache_filename(project: &Option<directories::ProjectDirs>, drive_id: &str) -> Option<std::path::PathBuf> {
    match project {
        Some(project) => {
            let mut cache_path = project.cache_dir().to_path_buf();
            if let Err(_) = std::fs::create_dir_all(&cache_path) {
                // let a later error sort it out
            }
            cache_path.push(format!("drive_{}", drive_id));
            Some(cache_path)
        }
        None => None
    }
}

struct Storage {
    path: Option<std::path::PathBuf>,
}

impl Storage {

    fn new(path: Option<std::path::PathBuf>) -> Storage {
        Storage {path}
    }

    fn load<T>(&self) -> Option<T>
        where T: serde::de::DeserializeOwned
    {
        if let Some(path) = &self.path {
            match std::fs::File::open(path) {
                Ok(file) => {
                    let reader = std::io::BufReader::new(file);
                    match serde_cbor::from_reader(reader) {
                        Ok(state) => {
                            return Some(state);
                        }
                        Err(error) => {
                            // storage file corrupted
                            eprintln!("{}\n", error);
                        }
                    }
                }
                Err(_) => {
                    // file does not exist, don't display an error for this common state.
                }
            }
        }
        None
    }

    fn save<T>(&self, state: &T)
        where T: serde::ser::Serialize
    {
        if let Some(path) = &self.path {
            let mut rng = thread_rng();
            let int = rng.gen_range(1000, 10000);
            let mut tmp_path = path.to_path_buf();
            assert!(tmp_path.set_extension(int.to_string()));
            match std::fs::File::create(&tmp_path) {
                Ok(file) => {
                    let result = {
                        let mut writer = std::io::BufWriter::new(file);
                        serde_cbor::to_writer(&mut writer, &state)
                    };
                    if let Err(error) = result {
                        eprintln!("{}\n", error);
                    }
                    else {
                        if let Err(error) = std::fs::rename(&tmp_path, path){
                            eprintln!("{}\n", error);
                        }
                        else {
                            return;
                        }
                    }
                    // tmp_path was created but not renamed.
                    if let Err(error) = std::fs::remove_file(&tmp_path){
                        eprintln!("{}\n", error);
                    }
                }
                Err(error) => {
                    eprintln!("{}\n", error);
                }
            }
        }
    }
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

fn sync_items(client: &reqwest::Client, mut snapshot: DriveSnapshot, bar: &indicatif::ProgressBar) -> Result<DriveSnapshot, Box<Error>> {
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

fn analyze_items(item_map: &HashMap<String, Item>)
    -> (u32, u32, BTreeMap<u64, HashMap<String, Vec<String>>>)
{
    let mut size_map = BTreeMap::<u64, HashMap<String, Vec<String>>>::new();
    let mut file_count = 0;
    let mut folder_count = 0;
    let bar = indicatif::ProgressBar::new(item_map.len() as u64);
    bar.set_style(indicatif::ProgressStyle::default_bar()
        .template("Analyzing duplicates: [{elapsed_precise}] {wide_bar} {percent}%")
        .progress_chars("#>-"));
    bar.tick();
    for item in item_map.values() {
        bar.inc(1);
        match &item.item_type {
            ItemType::File { mime_type, hashes } => {
                file_count += 1;
                if ignore_file(&mime_type) {
                    continue;
                }
                let dirname = match &item.parent {
                    Some(parent) => parent.path.trim_start_matches("/drive/root:/"),
                    None => {
                        eprintln!("Ignoring item due to missing or invalid 'parentReference': {:?}\n", item);
                        continue;
                    }
                };
                if ignore_path(dirname, &item.name) {
                    continue;
                }
                let sha1 = match hashes {
                    Some(hash) => &hash.sha,
                    None => {
                        eprintln!("Ignoring item due to missing or invalid 'sha1': {:?}\n", hashes);
                        continue;
                    }
                };
                let sha_map = size_map.entry(item.size).or_insert_with(HashMap::<String, Vec<String>>::new);
                // allocating the key only on insert is messy - we could use raw_entry here,
                // or maybe entry_ref() will exist one day - for now, always allocate
                let v = sha_map.entry(sha1.to_owned()).or_insert_with(Vec::<String>::new);
                let name = format!("{}/{}", dirname, item.name);
                v.push(name);
            }
            ItemType::Folder {} | ItemType::Package {} => {
                folder_count += 1;
            }
        }
    }
    bar.finish_and_clear();
    (file_count, folder_count, size_map)
}

fn main() {
    let project_dirs = directories::ProjectDirs::from("Casa", "Giddy", "MSOD-stat");
    let token = auth::authenticate(CLIENT_ID.to_owned(), CLIENT_SECRET.to_owned()).unwrap();
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::USER_AGENT,
        header::HeaderValue::from_str(
            &format!(
                "{}/{} reqwest/{}",
                CRATE_NAME.unwrap_or("msod-stat"),
                CRATE_VERSION.unwrap_or("unknown"),
                REQWEST_VERSION,
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
        let cache = Storage::new(cache_filename(&project_dirs, drive_id));
        let snapshot = cache.load().unwrap_or_else(|| DriveSnapshot::default(drive_id));
        bar.set_position(snapshot.state.size);
        let snapshot = sync_items(&client, snapshot, &bar).unwrap();
        cache.save(&snapshot);
        bar.finish_and_clear();
        let (file_count, folder_count, size_map) = analyze_items(&snapshot.state.items);
        println!("folders:{:>10}", folder_count);
        println!("files:  {:>10}", file_count);
        println!("duplicates:");
        for (size, sha_map) in size_map.iter().rev() {
            for names in sha_map.values() {
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
