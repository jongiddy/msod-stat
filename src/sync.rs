use std::error::Error;
use std::sync::mpsc;
use std::time::Duration;
use reqwest::StatusCode;
use serde_derive::Deserialize;


pub trait DriveItemHandler<DriveItem> {
    // remove all data and start from scratch
    fn reset(&mut self);

    // handle a received drive item
    fn handle(&mut self, item: DriveItem);
}

fn get(client: &reqwest::Client, uri: &str) -> Result<reqwest::Response, Box<Error>> {
    let mut retries = 3;
    let mut delay = 1;
    loop {
        match client.get(uri).send() {
            Ok(response) => {
                return Ok(response);
            }
            Err(ref error) if retries > 0 => {
                eprintln!("{:?}\n", error);
            },
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
    Done(String)
}
#[derive(Deserialize)]
struct SyncPage<DriveItem> {
    value: Vec<DriveItem>,
    #[serde(flatten)]
    link: SyncLink,
}

fn fetch_items<DriveItem>(
    client: &reqwest::Client,
    mut link: String,
    sender: mpsc::Sender<Option<Vec<DriveItem>>>
) -> String
    where DriveItem: serde::de::DeserializeOwned
{
    loop {
        match get(&client, &link) {
            Err(error) => {
                eprintln!("{}", error);
                panic!("Error fetching items");
            }
            Ok(mut response) => match response.status() {
                StatusCode::OK => {
                    match response.text() {
                        Ok(text) => {
                            let page: SyncPage<DriveItem> = match serde_json::from_str(&text) {
                                Ok(page) => page,
                                Err(error) => {
                                    eprintln!("{}", text);
                                    eprintln!("{}", error);
                                    panic!("Could not deserialize sync page")
                                }
                            };
                            sender.send(Some(page.value)).unwrap();
                            match page.link {
                                SyncLink::More(next) => {
                                    link = next;
                                },
                                SyncLink::Done(delta) => {
                                    return delta;
                                }
                            }
                        }
                        Err(error) => {
                            // error receiving full response, try again with same link
                            eprintln!("{}", error);
                        }
                    }
                },
                StatusCode::GONE => {
                    // If the server returns 410 Gone, the delta link has expired, and we need to
                    // start a new sync using the link in the Location header.
                    // https://docs.microsoft.com/onedrive/developer/rest-api/api/driveitem_delta#response-2
                    eprintln!("Delta link failed, restarting sync...");
                    // Send None to indicate that the DriveItemHandler should be reset
                    sender.send(None).unwrap();
                    link = response.headers().get("Location").unwrap().to_str().unwrap().to_owned();
                }
                status => {
                    // If the server returns a Retry-After header, then everything appears OK with
                    // the request, we just need to slow down.
                    // https://docs.microsoft.com/onedrive/developer/rest-api/concepts/scan-guidance#what-happens-when-you-get-throttled
                    match response.headers().get("Retry-After") {
                        Some(value) => {
                            let s = value.to_str().unwrap();
                            eprintln!("Status {:?}, Retry-After: {}\n", status, s);
                            let delay = s.parse().unwrap();
                            std::thread::sleep(Duration::from_secs(delay));
                        }
                        None => {
                            panic!("{:?} {}", status, status.canonical_reason().unwrap());
                        }
                    }
                }
            }
        }
    }
}

pub fn sync_drive_items<DriveItem: 'static>(
    client: &reqwest::Client,
    link: String,
    handler: &mut impl DriveItemHandler<DriveItem>
) -> Result<String, Box<Error>>
where DriveItem: Send + serde::de::DeserializeOwned
{
    let (sender, receiver) = mpsc::channel::<Option<Vec<DriveItem>>>();
    let client = client.clone();
    let t = std::thread::spawn(move || {
        fetch_items(&client, link, sender)
    });
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
    t.join().map_err(|any| string_error::into_err(format!("{:?}", any)))
}