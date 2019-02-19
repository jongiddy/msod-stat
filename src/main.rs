use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufRead, Write};
use std::sync::mpsc;
use std::time::Duration;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::{header, StatusCode};
use serde_json::{json, Value};
use oauth2::prelude::*;
use oauth2::basic::BasicTokenType;

mod auth;


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

fn start_fetcher(client: reqwest::Client, drive_id: &str)
    -> (std::thread::JoinHandle<reqwest::Client>, mpsc::Receiver<Option<Value>>)
{
    let mut link = format!("https://graph.microsoft.com/v1.0/me/drives/{}/root/delta", drive_id);
    let (sender, receiver) = mpsc::channel::<Option<Value>>();
    let t = std::thread::spawn(move || {
        loop {
            let result = get(&client, &link).unwrap();
            let mut json: Value = serde_json::from_str(&result).unwrap();
            sender.send(json.get_mut("value").map(Value::take)).unwrap();
            match json.get("@odata.nextLink") {
                Some(v) => {
                    link = v.as_str().unwrap().to_owned();
                },
                None => {
                    sender.send(None).unwrap();
                    break client;
                }
            }
        }
    });
    (t, receiver)
}

struct DriveSyncItemIterator {
    receiver: mpsc::Receiver<Option<Value>>,
    items: Value,
    item_index: usize,
}

impl DriveSyncItemIterator {
    fn new(receiver: mpsc::Receiver<Option<Value>>) -> DriveSyncItemIterator {
        let iter = DriveSyncItemIterator {
            receiver,
            items: json!([]),
            item_index: 0,
        };
        iter
    }
}

impl Iterator for DriveSyncItemIterator {
    // Iterator to return each drive item in turn.  Since items are obtained using the sync call,
    // the same item may occur multiple times, possibly with updated data.  If no value is ready
    // after 1 second, the iterator returns Err(mpsc::RecvTimeoutError::Timeout) to allow the
    // progress bar to be updated.
    type Item = Result<Value, mpsc::RecvTimeoutError>;

    fn next(&mut self) -> Option<Result<Value, mpsc::RecvTimeoutError>> {
        match &self.items {
            Value::Null => None,
            Value::Array(vec) => {
                let item_index = self.item_index + 1;
                if item_index < vec.len() {
                    self.item_index = item_index;
                    self.items.get_mut(item_index).map(Value::take).map(Ok)
                }
                else {
                    match self.receiver.recv_timeout(Duration::from_secs(1)) {
                        Ok(None) => {
                            self.items = Value::Null;
                            None
                        }
                        Ok(Some(items)) => {
                            self.items = items;
                            self.item_index = 0;
                            self.items.get_mut(0).map(Value::take).map(Ok)
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            Some(Err(mpsc::RecvTimeoutError::Timeout))
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            panic!("sender disconnected")
                        }
                    }
                }
            },
            v => {
                panic!("unexpected value: {:?}", v);
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

fn get_items(receiver: mpsc::Receiver<Option<Value>>, progress: &mut impl ProgressIndicator) -> HashMap<String, Value> {
    let mut id_map = HashMap::<String, Value>::new();
    progress.update();
    for result in DriveSyncItemIterator::new(receiver) {
        if let Ok(item) = result {
            let id = match item.get("id").and_then(Value::as_str) {
                Some(id) => id,
                None => {
                    eprintln!("Ignoring item due to missing or invalid 'id': {:?}", item);
                    continue;
                }
            };
            if let Some(prev) =
                if item.get("deleted").is_some() {
                    id_map.remove(id)
                }
                else {
                    progress.insert(&item);
                    id_map.insert(id.to_owned(), item)
                }
            {
                progress.delete(&prev);
            }
        }
        progress.update();
    }
    id_map
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

fn process_drive(receiver: mpsc::Receiver<Option<Value>>, progress: &mut impl ProgressIndicator)
    -> (u32, u32, BTreeMap<u64, HashMap<String, Vec<String>>>)
{
    let mut size_map = BTreeMap::<u64, HashMap<String, Vec<String>>>::new();
    let mut file_count = 0;
    let mut folder_count = 0;
    for item in get_items(receiver, progress).values() {
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

    let mut client = reqwest::Client::builder()
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
        let (thread, receiver) = start_fetcher(client, id);
        let (file_count, folder_count, size_map) = process_drive(receiver, &mut progress);
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
        client = thread.join().unwrap();
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
