extern crate open;
extern crate oauth2;
extern crate reqwest;
extern crate serde_json;
extern crate tiny_http;
extern crate url;

use std::io::{self, BufRead, Write};
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
    let mut response = client.get(uri).send()?;
    if response.status() != StatusCode::OK {
        panic!("{:?} {}", response.status(), response.status().canonical_reason().unwrap());
    }
    response.text()
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
                let mut json: Value = match get(self.client, &uri) {
                    Ok(result) => {
                        serde_json::from_str(&result).unwrap()
                    },
                    Err(err) => {
                        println!("Retrying: {:?}", err);
                        let result = get(self.client, &uri).unwrap();
                        serde_json::from_str(&result).unwrap()
                    }
                };
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

fn process_drive(client: &reqwest::Client, drive_id: &str) -> (u32, u32) {
    let mut seen = std::collections::HashSet::<String>::new();
    let mut file_count = 0;
    let mut folder_count = 0;
    for mut item in DriveSyncItemIterator::new(client, drive_id) {
        if item.get("deleted").is_some() {
            println!("{:?}", item);
            continue;
        }
        let mut id_value = item.get_mut("id").map(Value::take).unwrap();
        let id = id_value.as_str().unwrap();
        if seen.contains(id) {
            continue;
        }
        seen.insert(id.to_owned());
        if item.get("file").is_some() {
            file_count += 1;
        }
        else if item.get("folder").is_some() || item.get("package").is_some() {
            folder_count += 1;
            print!(".");
            io::stdout().flush().unwrap();
        }
        else {
            print!("(ignoring {})", item["name"].as_str().unwrap());
        }
    }
    (file_count, folder_count)
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
    println!("{:?}", token);
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
        .timeout(std::time::Duration::from_secs(300))
        .default_headers(headers)
        .build().unwrap();

    let mut response = client.get("https://graph.microsoft.com/v1.0/me/drives").send().unwrap();
    if response.status() != StatusCode::OK {
        panic!("{:?} {}", response.status(), response.status().canonical_reason().unwrap());
    }
    let result = response.text().unwrap();
    println!("{:?}", response);
    println!("{}", result);
    let json: Value = serde_json::from_str(&result).unwrap();
    let mut drive_ids = vec![];
    for drive in json["value"].as_array().unwrap() {
        let id = drive["id"].as_str().unwrap();
        let (file_count, folder_count) = process_drive(&client, id);
        drive_ids.push(id.to_string());
        let quota = &drive["quota"];
        let total = quota["total"].as_u64().unwrap();
        let used = quota["used"].as_u64().unwrap();
        let deleted = quota["deleted"].as_u64().unwrap();
        let remaining = quota["remaining"].as_u64().unwrap();
        assert!(used + remaining == total);
        println!();
        println!("Drive {}", id);
        println!("folders:{:>10}", folder_count);
        println!("files:  {:>10}", file_count);
        println!("total:  {:>18}", size_as_string(total));
        println!("free:   {:>18}", size_as_string(remaining));
        println!(
            "used:   {:>18} = {:.2}% (including {} pending deletion)",
            size_as_string(used),
            used as f32 * 100.0 / total as f32,
            size_as_string(deleted)
        );
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
