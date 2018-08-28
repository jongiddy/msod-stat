extern crate open;
extern crate oauth2;
extern crate reqwest;
extern crate serde_json;
extern crate tiny_http;
extern crate url;

use std::io::{self, BufRead};
use reqwest::{header, StatusCode};
use serde_json::Value;
use oauth2::prelude::*;
use oauth2::basic::BasicTokenType;

mod auth;


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
    let mut headers = header::Headers::new();
    match token.token_type() {
        BasicTokenType::Bearer => {
            headers.set(
                header::Authorization(
                    header::Bearer {
                       token: token.access_token().secret().to_string()
                   }
               )
            );
        },
        BasicTokenType::Mac => {
            panic!("reqwest does not support MAC Authorization")
        }
    }

    let client = reqwest::Client::builder().default_headers(headers).build().unwrap();

    let mut response = client.get("https://graph.microsoft.com/v1.0/me/drives").send().unwrap();
    if response.status() != StatusCode::Ok {
        panic!("{:?} {}", response.status(), response.status().canonical_reason().unwrap());
    }
    let result = response.text().unwrap();
    println!("{:?}", response);
    println!("{}", result);
    let json: Value = serde_json::from_str(&result).unwrap();
    for drive in json["value"].as_array().unwrap() {
        let quota = &drive["quota"];
        let total = quota["total"].as_u64().unwrap();
        let used = quota["used"].as_u64().unwrap();
        let deleted = quota["deleted"].as_u64().unwrap();
        let remaining = quota["remaining"].as_u64().unwrap();
        assert!(used + remaining == total);
        println!("Drive {}", drive["id"].as_str().unwrap());
        println!("total:\t{:>18}", size_as_string(total));
        println!("free:\t{:>18}", size_as_string(remaining));
        println!(
            "used:\t{:>18} = {:.2}% (including {} pending deletion)",
            size_as_string(used),
            used as f32 * 100.0 / total as f32,
            size_as_string(deleted)
        );
        println!();
    }
}

fn size_as_string(value: u64) -> String {
    let mib = value as f32 / 1024.0 / 1024.0;
    if mib < 1000.0 {
        format!("{:.3}MiB", mib)
    }
    else {
        let gib = mib / 1024.0;
        format!("{:.3}GiB", gib)
    }
}
