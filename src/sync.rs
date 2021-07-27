use eyre::{eyre, Result};
use reqwest::blocking::{Client, Response};
use reqwest::header::RETRY_AFTER;
use reqwest::StatusCode;
use serde_derive::Deserialize;
use serde_json::Value;
use std::panic;
use std::sync::mpsc;
use std::time::Duration;

pub trait DriveItemHandler<DriveItem> {
    // remove all data and start from scratch
    fn reset(&mut self);

    // handle a received drive item
    fn handle(&mut self, item: DriveItem);
}

fn get(client: &Client, uri: &str) -> Result<Response> {
    let mut retries = 3;
    let mut delay = 1;
    loop {
        match client.get(uri).send() {
            Ok(response) => {
                return Ok(response);
            }
            Err(ref error) if retries > 0 => {
                eprintln!("{:?}\n", error);
            }
            Err(error) => {
                return Err(error.into());
            }
        }
        std::thread::sleep(Duration::from_secs(delay));
        retries -= 1;
        delay *= 16;
    }
}

#[derive(Deserialize)]
enum SyncLink {
    #[serde(rename = "@odata.nextLink")]
    More(String),
    #[serde(rename = "@odata.deltaLink")]
    Done(String),
}
#[derive(Deserialize)]
struct SyncPage<DriveItem> {
    value: Vec<DriveItem>,
    #[serde(flatten)]
    link: SyncLink,
}

macro_rules! retry_or_panic {
    ( $count:ident, $message:expr ) => {
        if $count < 3 {
            $count += 1;
            // extra newline to avoid overwrite by progress bar
            eprintln!("Retry After: 30 ({})\n", $message);
            std::thread::sleep(Duration::from_secs(30));
        } else {
            panic!($message);
        }
    };
}

fn fetch_items<DriveItem>(
    client: &Client,
    reset_link: String,
    mut link: String,
    sender: mpsc::Sender<Option<Vec<DriveItem>>>,
) -> String
where
    DriveItem: serde::de::DeserializeOwned,
{
    let mut fail_count = 0;
    loop {
        match get(&client, &link) {
            Err(error) => {
                eprintln!("{}", error);
                retry_or_panic!(fail_count, "Error fetching items");
            }
            Ok(response) => match response.status() {
                StatusCode::OK => {
                    match response.text() {
                        Ok(text) => {
                            match serde_json::from_str::<SyncPage<DriveItem>>(&text) {
                                Ok(page) => {
                                    sender.send(Some(page.value)).unwrap();
                                    match page.link {
                                        SyncLink::More(next) => {
                                            fail_count = 0;
                                            link = next;
                                        }
                                        SyncLink::Done(delta) => {
                                            return delta;
                                        }
                                    }
                                }
                                Err(error) => {
                                    eprintln!("{}", error);
                                    eprintln!("{}", text);
                                    retry_or_panic!(fail_count, "Could not deserialize sync page");
                                }
                            };
                        }
                        Err(error) => {
                            // error receiving full response, try again with same link
                            eprintln!("{}", error);
                            retry_or_panic!(fail_count, "Partial response");
                        }
                    }
                }
                StatusCode::GONE | StatusCode::UNAUTHORIZED => {
                    // If the server returns 410 Gone, the delta link has expired. Start a new sync
                    // using the link in the Location header:
                    // https://docs.microsoft.com/onedrive/developer/rest-api/api/driveitem_delta#response-2
                    // Although not documented, the API can return 401 Unauthorized when using an
                    // old, but correctly authorized, delta link: https://github.com/jongiddy/msod-stat/issues/1
                    eprintln!("Delta link failed, restarting sync...");
                    // Send None to indicate that the DriveItemHandler should be reset
                    sender.send(None).unwrap();
                    link = match response.headers().get("Location") {
                        Some(location) => match location.to_str() {
                            Ok(s) => s.to_owned(),
                            Err(_) => reset_link.clone(),
                        },
                        None => reset_link.clone(),
                    };
                }
                status => {
                    eprintln!(
                        "Response {:?} {}",
                        status,
                        status.canonical_reason().unwrap()
                    );
                    let retry_header = response
                        .headers()
                        .get(RETRY_AFTER)
                        .map(|v| v.to_str().unwrap().to_string());
                    match response.text() {
                        Ok(text) => {
                            eprintln!("Text: {}", text);
                            match serde_json::from_str::<Value>(&text) {
                                Ok(page) => match page.get("error") {
                                    Some(error) => {
                                        if let Some(code) =
                                            error.get("code").and_then(Value::as_str)
                                        {
                                            eprintln!("Code: {}", code);
                                        }
                                        if let Some(message) =
                                            error.get("message").and_then(Value::as_str)
                                        {
                                            if message.len() > 0 {
                                                eprintln!("Message: {}", message);
                                            }
                                        }
                                    }
                                    None => {
                                        eprintln!("Text: {:?}", text);
                                    }
                                },
                                Err(error) => {
                                    eprintln!("Text: {:?}", text);
                                    eprintln!("{}", error);
                                }
                            };
                        }
                        Err(error) => {
                            eprintln!("{}", error);
                        }
                    }
                    // If the server returns a Retry-After header, then everything appears OK with
                    // the request, we just need to slow down.
                    // https://docs.microsoft.com/onedrive/developer/rest-api/concepts/scan-guidance#what-happens-when-you-get-throttled
                    match retry_header {
                        Some(s) => {
                            eprintln!("Retry-After: {}\n", s);
                            let delay = s.parse().unwrap();
                            std::thread::sleep(Duration::from_secs(delay));
                        }
                        None => {
                            retry_or_panic!(fail_count, "Unexpected response");
                        }
                    }
                }
            },
        }
    }
}

pub fn sync_drive_items<DriveItem: 'static>(
    client: &Client,
    reset_link: String,
    link: String,
    handler: &mut impl DriveItemHandler<DriveItem>,
) -> Result<String>
where
    DriveItem: Send + serde::de::DeserializeOwned,
{
    let (sender, receiver) = mpsc::channel::<Option<Vec<DriveItem>>>();
    let client = client.clone();
    let t = std::thread::spawn(move || fetch_items(&client, reset_link, link, sender));
    loop {
        match receiver.recv() {
            Ok(Some(items)) => {
                for item in items.into_iter() {
                    handler.handle(item);
                }
            }
            Ok(None) => {
                // None indicates that the sender thread has had to restart the sync from the beginning.
                handler.reset();
            }
            Err(mpsc::RecvError) => {
                // RecvError means that the sender has closed the channel. This only happens
                // when there are no more pages or the sending thread has panicked.
                break;
            }
        }
    }
    match t.join() {
        Ok(delta_link) => Ok(delta_link),
        Err(err) => {
            match err.downcast::<&str>() {
                Ok(s) => {
                    Err(eyre!(s))
                }
                Err(err) => {
                    panic::resume_unwind(err)
                }
            }
        }
    }
}
