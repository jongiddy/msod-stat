use crate::id_item_map::{get_id_item_map, ProgressIndicator};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;
use reqwest::{header, StatusCode};
use serde_json::Value;
use oauth2::prelude::*;
use oauth2::basic::BasicTokenType;

mod auth;
mod id_item_map;

// Making the OAuth2 client secret public is secure because PKCE ensures
// that only the originator can use the authorization code.
const CLIENT_ID: &str = "6612d641-e7d8-4d39-8dac-e6f21efe1bf4";
const CLIENT_SECRET: &str = "ubnDYPYV4019]pentXO1~[=";

struct FetchDataProgressBar {
    bar: indicatif::ProgressBar,
    total: u64,
}

impl FetchDataProgressBar {

    fn new(used: u64) -> FetchDataProgressBar {
        let bar = indicatif::ProgressBar::new(used);
        bar.set_style(indicatif::ProgressStyle::default_bar()
            .template("Fetching drive data: [{elapsed_precise}] {wide_bar} {percent}%")
            .progress_chars("#>-"));
        bar.tick();
        FetchDataProgressBar {
            bar,
            total: 0u64,
        }
    }

    fn close(self) {
        self.bar.finish_and_clear();
    }
}

impl ProgressIndicator for FetchDataProgressBar {
    fn update(&self) {
        self.bar.set_position(self.total);
    }

    fn insert(&mut self, item: &Value) {
        if item.get("file").is_some() {
            let size = item.get("size").and_then(Value::as_u64).unwrap_or(0);
            self.total += size;
        }
    }

    fn delete(&mut self, prev: &Value) {
        if prev.get("file").is_some() {
            let size = prev.get("size").and_then(Value::as_u64).unwrap_or(0);
            assert!(size <= self.total);
            self.total -= size;
        }
    }
}

fn ignore_file(file: &Value) -> bool {
    // Files with the "application/msonenote" MIME Type do not have a SHA
    file.get("mimeType").and_then(Value::as_str).map_or(false, |s| s == "application/msonenote")
}

fn ignore_path(dirname: &str, basename: &str) -> bool {
    // SVN repo files may be duplicated in the .svn directory. Don't match these,
    // as they are part of the SVN repo format, and should not be modified
    // individually.
    basename.ends_with(".svn-base") && dirname.contains("/.svn/pristine/")
}

fn process_drive(item_map: &HashMap<String, Value>)
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
        if let Some(file) = item.get("file") {
            file_count += 1;
            if ignore_file(&file) {
                continue;
            }
            let basename = match item.get("name").and_then(Value::as_str) {
                Some(name) => name,
                None => {
                    eprintln!("Ignoring item due to missing or invalid 'name': {:?}", item);
                    continue;
                }
            };
            let dirname = match item.get("parentReference")
                    .and_then(|v| v.get("path"))
                    .and_then(Value::as_str) {
                Some(path) => path.trim_start_matches("/drive/root:/"),
                None => {
                    eprintln!("Ignoring item due to missing or invalid 'parentReference': {:?}", item);
                    continue;
                }
            };
            if ignore_path(dirname, basename) {
                continue;
            }
            let size = match item.get("size").and_then(Value::as_u64) {
                Some(size) => size,
                None => {
                    eprintln!("Ignoring item due to missing or invalid 'size': {:?}", item);
                    continue;
                }
            };
            let sha1 = match file.get("hashes")
                    .and_then(|v| v.get("sha1Hash"))
                    .and_then(Value::as_str) {
                Some(sha1) => sha1,
                None => {
                    eprintln!("Ignoring item due to missing or invalid 'sha1': {:?}", file);
                    continue;
                }
            };
            let sha_map = size_map.entry(size).or_insert_with(HashMap::<String, Vec<String>>::new);
            // allocating the key only on insert is messy - we could use raw_entry here,
            // or maybe entry_ref() will exist one day - for now, always allocate
            let v = sha_map.entry(sha1.to_owned()).or_insert_with(Vec::<String>::new);
            let name = format!("{}/{}", dirname, basename);
            v.push(name);
        }
        else if item.get("folder").is_some() || item.get("package").is_some() {
            folder_count += 1;
        }
        else {
            eprintln!("Ignoring unrecognized item: {:?}", item);
        }
    }
    bar.finish_and_clear();
    (file_count, folder_count, size_map)
}

fn main() {
    let token = auth::authenticate(CLIENT_ID.to_owned(), CLIENT_SECRET.to_owned()).unwrap();
    let mut headers = header::HeaderMap::new();
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
        let mut progress = FetchDataProgressBar::new(used);
        let item_map = get_id_item_map(&client, drive_id, &mut progress);
        progress.close();
        let (file_count, folder_count, size_map) = process_drive(&item_map);
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
