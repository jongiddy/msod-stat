extern crate open;
extern crate oauth2;
extern crate reqwest;
extern crate tiny_http;
extern crate url;

use std::io::{self, BufRead};
use reqwest::{header, StatusCode};
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
}
