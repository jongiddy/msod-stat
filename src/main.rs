mod auth;
mod item;
mod size;
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

use crate::item::{initial_link, DriveSnapshot, DriveState, Item};
use crate::size::{bucket_by_size, size_as_string};
use crate::storage::Storage;
use crate::sync::{sync_drive_items, DriveItemHandler};
use eyre::{Report, Result, bail, ensure};
use oauth2::basic::BasicTokenType;
use oauth2::TokenResponse;
use reqwest::blocking::Client;
use reqwest::{header, StatusCode};
use serde_json::Value;
use std::time::Duration;

const CRATE_NAME: Option<&str> = option_env!("CARGO_PKG_NAME");
const CRATE_VERSION: Option<&str> = option_env!("CARGO_PKG_VERSION");

// To replace this client ID, register a Public client/native application in Azure Active Directory.
// See https://docs.microsoft.com/azure/active-directory/develop/quickstart-register-app
// Under `Authentication` set the redirect URI to `http://localhost/redirect` and enable `Allow public client flows`.
// Under `API permissions` add Microsoft Graph delegated permission `Files.Read.All`.
// Add the `Application (client) ID` as the `CLIENT_ID` below.
const CLIENT_ID: &str = "3a139972-0147-433a-9ab8-faa3dd1b9eb5";

fn cache_filename(project: &directories::ProjectDirs, drive_id: &str) -> std::path::PathBuf {
    let mut cache_path = project.cache_dir().to_path_buf();
    if let Err(_) = std::fs::create_dir_all(&cache_path) {
        // let a later error sort it out
    }
    // Increment the number after `drive` when the serialized format changes.
    // 2021-05-23 - updated to 2 because the original delta link format is no longer valid
    // 2021-06-05 - remove mime type from saved data
    cache_path.push(format!("drive3_{}", drive_id));
    cache_path.set_extension("cbor");
    cache_path
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
        } else {
            self.state.upsert(item)
        };
        self.bar.set_position(size);
    }
}

fn sync_items(
    client: &Client,
    mut snapshot: DriveSnapshot,
    reset_link: String,
    bar: &indicatif::ProgressBar,
) -> Result<DriveSnapshot> {
    let mut handler = ItemHandler {
        state: &mut snapshot.state,
        bar,
    };
    snapshot.delta_link = sync_drive_items(client, reset_link, snapshot.delta_link, &mut handler)?;
    Ok(snapshot)
}

fn get_msgraph_client() -> Result<Client> {
    let token = auth::authenticate(CLIENT_ID.to_owned())?;
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::USER_AGENT,
        header::HeaderValue::from_str(&format!(
            "{}/{}",
            CRATE_NAME.unwrap_or("msod-stat"),
            CRATE_VERSION.unwrap_or("unknown"),
        ))?,
    );
    match token.token_type() {
        BasicTokenType::Bearer => {
            headers.insert(
                header::AUTHORIZATION,
                header::HeaderValue::from_str(&format!(
                    "Bearer {}",
                    token.access_token().secret().to_string()
                ))?,
            );
        }
        _ => {
            bail!("only support Bearer Authorization")
        }
    }
    Client::builder()
        .timeout(Duration::from_secs(120))
        .default_headers(headers)
        .build()
        .map_err(Report::new)
}

fn fetch_drive(
    drive_id: &str,
    expected: u64,
    project_dirs: &Option<directories::ProjectDirs>,
    client: &Client,
) -> Result<DriveSnapshot> {
    let bar = indicatif::ProgressBar::new(expected);
    bar.set_style(
        indicatif::ProgressStyle::default_bar()
            .template("Fetching drive data: [{elapsed_precise}] {wide_bar} {percent}%")
            .progress_chars("#>-"),
    );
    bar.enable_steady_tick(100);
    let cache = Storage::new(
        project_dirs
            .as_ref()
            .map(|dir| cache_filename(dir, drive_id)),
    );
    let snapshot = cache
        .load()
        .unwrap_or_else(|| DriveSnapshot::default(drive_id));
    bar.set_position(snapshot.state.size);
    let snapshot = sync_items(client, snapshot, initial_link(drive_id), &bar)?;
    cache.save(&snapshot);
    bar.finish_and_clear();
    Ok(snapshot)
}

fn show_usage(drive: &Value) {
    let quota = &drive["quota"];
    let total = quota["total"].as_u64().unwrap();
    let used = quota["used"].as_u64().unwrap();
    let deleted = quota["deleted"].as_u64().unwrap();
    let remaining = quota["remaining"].as_u64().unwrap();
    assert!(used + remaining == total);
    println!("total:  {:>18}", size_as_string(total));
    println!("free:   {:>18}", size_as_string(remaining));
    println!(
        "used:   {:>18} = {:.2}% (including {} pending deletion)",
        size_as_string(used),
        used as f32 * 100.0 / total as f32,
        size_as_string(deleted)
    );
}

fn show_duplicates(snapshot: DriveSnapshot) {
    let (file_count, folder_count, names_by_hash_by_size) = bucket_by_size(&snapshot.state.items);
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

fn main() -> Result<()> {
    let project_dirs = directories::ProjectDirs::from("Casa", "Giddy", "MSOD-stat");
    let client = get_msgraph_client()?;
    let response = client
        .get("https://graph.microsoft.com/v1.0/me/drives")
        .send()?;
    ensure!(
        response.status() == StatusCode::OK,
        "{:?} {}",
        response.status(),
        response.status().canonical_reason().unwrap()
    );
    let result = response.text()?;
    let json: Value = serde_json::from_str(&result)?;
    for drive in json["value"].as_array().unwrap() {
        let drive_id = drive["id"].as_str().unwrap();
        println!();
        println!("Drive {}", drive_id);
        show_usage(drive);
        let snapshot = fetch_drive(
            drive_id,
            drive["quota"]["used"].as_u64().unwrap(),
            &project_dirs,
            &client,
        )?;
        show_duplicates(snapshot);
    }
    Ok(())
}
