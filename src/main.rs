use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufRead, Write};
use std::time::Duration;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::{header, StatusCode};
use serde_json::{json, Value};
use oauth2::prelude::*;
use oauth2::basic::BasicTokenType;

mod auth;


struct DriveSyncPageIterator<'a> {
    client: &'a reqwest::Client,
    next_link: Option<String>,
}

impl<'a> DriveSyncPageIterator<'a> {
    fn new<'b>(client: &'b reqwest::Client, drive_id: &str) -> DriveSyncPageIterator<'b> {
        let link = format!("https://graph.microsoft.com/v1.0/me/drives/{}/root/delta", drive_id);
        DriveSyncPageIterator {
            client: client,
            next_link: Some(link),
        }
    }
}

fn get(client: &reqwest::Client, uri: &str) -> Result<String, reqwest::Error> {
    let mut retries = 3;
    let mut default_delay = 1;
    loop {
        let retry_delay = Duration::from_secs(
            match client.get(uri).send() {
                Ok(mut response) => match response.status() {
                    StatusCode::OK => {
                        return response.text();
                    },
                    status if retries > 0 => {
                        let delay = match response.headers().get("Retry-After") {
                            Some(value) => {
                                value.to_str().unwrap().parse().unwrap()
                            },
                            None => {
                                default_delay
                            }
                        };
                        println!("HTTP status {}...", status);
                        io::stdout().flush().unwrap();
                        delay
                    },
                    status => {
                        panic!("{:?} {}", status, status.canonical_reason().unwrap());
                    }
                },
                Err(ref err) if retries > 0 => {
                    println!("{:?}", err);
                    default_delay
                },
                Err(err) => {
                    return Err(err);
                }
            }
        );
        std::thread::sleep(retry_delay);
        retries -= 1;
        default_delay *= 16;
    }
}

impl<'a> Iterator for DriveSyncPageIterator<'a> {
    type Item = Value;

    fn next(&mut self) -> Option<Value> {
        let next_link = std::mem::replace(&mut self.next_link, None);
        match next_link {
            None => {
                None
            },
            Some(uri) => {
                let result = get(self.client, &uri).unwrap();
                let mut json: Value = serde_json::from_str(&result).unwrap();
                self.next_link = json.get("@odata.nextLink").map(|v| v.as_str().unwrap().to_owned());
                json.get_mut("value").map(Value::take)
            }
        }
    }
}

struct DriveSyncItemIterator<'a> {
    page_iter: DriveSyncPageIterator<'a>,
    items: Value,
    item_index: usize,
}

impl<'a> DriveSyncItemIterator<'a> {
    fn new<'b>(client: &'b reqwest::Client, drive_id: &str) -> DriveSyncItemIterator<'b> {
        DriveSyncItemIterator {
            page_iter: DriveSyncPageIterator::new(client, drive_id),
            items: json!([]),
            item_index: 1,
        }
    }
}

impl<'a> Iterator for DriveSyncItemIterator<'a> {
    type Item = Value;

    fn next(&mut self) -> Option<Value> {
        match self.item_index {
            0 => None,
            _ => {
                if self.item_index >= self.items.as_array().unwrap().len() {
                    let value = self.page_iter.next();
                    match value {
                        None => {
                            None
                        },
                        Some(items) => {
                            self.items = items;
                            self.item_index = 1;
                            self.items.get_mut(0).map(Value::take)
                        }
                    }
                }
                else {
                    let val = self.items.get_mut(self.item_index).map(Value::take);
                    self.item_index += 1;
                    val
                }
            }
        }
    }
}

trait ProgressIndicator {
    fn update(&self);

    fn insert(&mut self, item: &Value);

    fn delete(&mut self, prev: &Value);
}

struct IndicatifProgressBar {
    bar: ProgressBar,
    total: u64,
}

impl IndicatifProgressBar {

    fn new(used: u64) -> IndicatifProgressBar {
        let bar = ProgressBar::new(used);
        bar.set_style(ProgressStyle::default_bar()
            .template("Analyzing duplicates: [{elapsed_precise}] {wide_bar} {percent}%")
            .progress_chars("#>-"));
        bar.tick();
        IndicatifProgressBar {
            bar,
            total: 0u64,
        }
    }

    fn close(self) {
        self.bar.finish_and_clear();
    }
}

impl ProgressIndicator for IndicatifProgressBar {
    fn update(&self) {
        self.bar.set_position(self.total);
    }

    fn insert(&mut self, item: &Value) {
        if item.get("file").is_some() {
            let size = item.get("size").unwrap().as_u64().unwrap();
            self.total += size;
        }
    }

    fn delete(&mut self, prev: &Value) {
        if prev.get("file").is_some() {
            let size = prev.get("size").unwrap().as_u64().unwrap();
            assert!(size <= self.total);
            self.total -= size;
        }
    }
}

fn get_items(client: &reqwest::Client, drive_id: &str, progress: &mut impl ProgressIndicator) -> HashMap<String, Value> {
    let mut id_map = HashMap::<String, Value>::new();
    progress.update();
    for item in DriveSyncItemIterator::new(client, drive_id) {
        let id = item.get("id").unwrap().as_str().unwrap();
        if item.get("deleted").is_some() {
            if let Some(prev) = id_map.remove(id) {
                progress.delete(&prev);
            }
        }
        else {
            progress.insert(&item);
            if let Some(prev) = id_map.insert(id.to_owned(), item) {
                progress.delete(&prev);
            }
        }
        progress.update();
    }
    id_map
}

fn process_drive(client: &reqwest::Client, drive_id: &str, progress: &mut impl ProgressIndicator)
    -> (u32, u32, BTreeMap<u64, HashMap<String, Vec<String>>>)
{
    let mut size_map = BTreeMap::<u64, HashMap<String, Vec<String>>>::new();
    let mut file_count = 0;
    let mut folder_count = 0;
    for item in get_items(client, drive_id, progress).values() {
        if let Some(file) = item.get("file") {
            file_count += 1;
            let size = item.get("size").unwrap().as_u64().unwrap();
            if file.get("mimeType").unwrap().as_str().unwrap() != "application/msonenote" {
                let sha1 = file.get("hashes").unwrap().get("sha1Hash").unwrap().as_str().unwrap();
                let sha_map = size_map.entry(size).or_insert_with(HashMap::<String, Vec<String>>::new);
                // allocating the key only on insert is messy - we could use raw_entry here,
                // or maybe entry_ref() will exist one day - for now, always allocate
                let v = sha_map.entry(sha1.to_owned()).or_insert_with(Vec::new);
                let basename = item.get("name").unwrap().as_str().unwrap();
                let dirname = item.get("parentReference").unwrap()
                    .get("path").unwrap().as_str().unwrap().trim_start_matches("/drive/root:/");
                let name = format!("{}/{}", dirname, basename);
                v.push(name);
            }
        }
        else if item.get("folder").is_some() || item.get("package").is_some() {
            folder_count += 1;
        }
        else {
            print!("(ignoring {})", item["name"].as_str().unwrap());
        }
    }
    (file_count, folder_count, size_map)
}

fn main() {
    // See https://docs.microsoft.com/en-us/onedrive/developer/rest-api/getting-started/graph-oauth
    // To get a username/password for an app:
    // 1. Go to https://apps.dev.microsoft.com/
    // 2. Click Add an App.
    // 3. Skip the guided setup.
    // 4. Set Web Redirect URL to http://localhost:3000/redirect
    // 5. Add Delegated Permissions of Files.Read.All
    // 6. Copy the Application Id as the username.
    // 7. Click Generate New Password.
    // 8. Copy the password.
    // 9. Create a credentials file containing the username and password on separate lines.
    // 10. Pipe the credentials file into this command.

    let mut username = String::new();
    let mut password = String::new();
    let stdin = io::stdin();
    {
        let mut buf = stdin.lock();
        buf.read_line(&mut username).unwrap();
        buf.read_line(&mut password).unwrap();
        username.pop();
        password.pop();
    }
    let token = auth::authenticate(username, password).unwrap();
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
        let id = drive["id"].as_str().unwrap();
        let quota = &drive["quota"];
        let total = quota["total"].as_u64().unwrap();
        let used = quota["used"].as_u64().unwrap();
        let deleted = quota["deleted"].as_u64().unwrap();
        let remaining = quota["remaining"].as_u64().unwrap();
        assert!(used + remaining == total);
        println!();
        println!("Drive {}", id);
        println!("total:  {:>18}", size_as_string(total));
        println!("free:   {:>18}", size_as_string(remaining));
        println!(
            "used:   {:>18} = {:.2}% (including {} pending deletion)",
            size_as_string(used),
            used as f32 * 100.0 / total as f32,
            size_as_string(deleted)
        );
        let mut progress = IndicatifProgressBar::new(used);
        let (file_count, folder_count, size_map) = process_drive(&client, id, &mut progress);
        progress.close();
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
