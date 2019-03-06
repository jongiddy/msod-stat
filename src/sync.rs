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

fn get(client: &reqwest::Client, uri: &str) -> Result<String, String> {
    let mut retries = 3;
    let mut default_delay = 1;
    loop {
        let retry_delay = Duration::from_secs(
            match client.get(uri).send() {
                Ok(mut response) => match response.status() {
                    StatusCode::OK => {
                        match response.text() {
                            Ok(text) => {
                                return Ok(text);
                            }
                            Err(ref error) if retries > 0 => {
                                eprintln!("{}\n", error);
                                default_delay
                            }
                            Err(error) => {
                                panic!(error);
                            }
                        }
                    },
                    StatusCode::GONE => {
                        // a delta link may have expired, in which case OneDrive will return
                        // 410 Gone and a Location header with a new nextLink. This needs to
                        // be handled higher up.
                        return Err(response.headers().get("Location").unwrap().to_str().unwrap().to_owned());
                    }
                    status if retries > 0 => {
                        let delay = match response.headers().get("Retry-After") {
                            Some(value) => {
                                match value.to_str() {
                                    Ok(value) => {
                                        value.parse().unwrap_or(default_delay)
                                    },
                                    Err(error) => {
                                        eprintln!("{}\n", error);
                                        default_delay
                                    }
                                }
                            },
                            None => {
                                default_delay
                            }
                        };
                        eprintln!("HTTP status {}...\n", status);
                        delay
                    },
                    status => {
                        panic!("{:?} {}", status, status.canonical_reason().unwrap());
                    }
                },
                Err(ref error) if retries > 0 => {
                    eprintln!("{:?}\n", error);
                    default_delay
                },
                Err(error) => {
                    panic!("{}", error);
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
            Ok(result) => {
                let page: SyncPage<DriveItem> = match serde_json::from_str(&result) {
                    Ok(page) => page,
                    Err(error) => {
                        eprintln!("{}", result);
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
            Err(next) => {
                eprintln!("Delta link failed, restarting sync...");
                // indicate that the DriveItemHandler should be reset
                sender.send(None).unwrap();
                link = next;
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