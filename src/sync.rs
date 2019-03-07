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

#[derive(Debug, Clone)]
struct StatusCodeError {
    status: StatusCode
}

impl std::fmt::Display for StatusCodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?} {}", self.status, self.status.canonical_reason().unwrap())
    }
}

impl Error for StatusCodeError {
    fn description(&self) -> &str {
        "status code error"
    }

    fn cause(&self) -> Option<&Error> {
        // Generic error, underlying cause isn't tracked.
        None
    }
}

fn get(client: &reqwest::Client, uri: &str) -> Result<reqwest::Response, Box<Error>> {
    let mut retries = 3;
    let mut default_delay = 1;
    loop {
        let retry_delay = Duration::from_secs(
            match client.get(uri).send() {
                Ok(response) => {
                    return Ok(response);
                }
                Err(ref error) if retries > 0 => {
                    eprintln!("{:?}\n", error);
                    default_delay
                },
                Err(error) => {
                    return Err(error.into());
                }
            }
        );
        std::thread::sleep(retry_delay);
        retries -= 1;
        default_delay *= 16;
    }
}

#[derive(Deserialize)]
enum SyncLink {
    #[serde(rename = "@odata.nextLink")]
    Next(String),
    #[serde(rename = "@odata.deltaLink")]
    Delta(String)
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
    sender: mpsc::Sender<Option<DriveItem>>
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
                            for value in page.value.into_iter() {
                                sender.send(Some(value)).unwrap();
                            }
                            match page.link {
                                SyncLink::Next(next) => {
                                    link = next;
                                },
                                SyncLink::Delta(delta) => {
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
                    // if the delta link has expired, OneDrive will return
                    // 410 Gone and a Location header with a new nextLink,
                    // but we start from the beginning of the sync.
                    eprintln!("Delta link failed, restarting sync...");
                    // indicate that the DriveItemHandler should be reset
                    sender.send(None).unwrap();
                    link = response.headers().get("Location").unwrap().to_str().unwrap().to_owned();
                }
                status => {
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
    let (sender, receiver) = mpsc::channel::<Option<DriveItem>>();
    let client = client.clone();
    let t = std::thread::spawn(move || {
        fetch_items(&client, link, sender)
    });
    loop {
        match receiver.recv() {
            Ok(Some(item)) => {
                handler.handle(item);
            }
            Ok(None) => {
                handler.reset();
            }
            Err(mpsc::RecvError) => {
                break;
            }
        }
    }
    t.join().map_err(|any| string_error::into_err(format!("{:?}", any)))
}