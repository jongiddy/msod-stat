use std::error::Error;
use std::sync::mpsc;
use std::time::Duration;
use reqwest::StatusCode;
use serde_derive::Deserialize;


pub trait DriveItemHandler<DriveItem> {
    fn tick(&self);

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


fn get(client: &reqwest::Client, uri: &str) -> String {
    let mut retries = 3;
    let mut default_delay = 1;
    loop {
        let retry_delay = Duration::from_secs(
            match client.get(uri).send() {
                Ok(mut response) => match response.status() {
                    StatusCode::OK => {
                        return response.text().unwrap();
                    },
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
struct SyncPage<DriveItem> {
    value: Vec<DriveItem>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
}

fn fetch_items<DriveItem>(
    client: &reqwest::Client,
    mut link: String,
    sender: mpsc::Sender<DriveItem>
)
where DriveItem: serde::de::DeserializeOwned
{
    loop {
        let result = get(&client, &link);
        let page: SyncPage<DriveItem> = serde_json::from_str(&result).unwrap();
        for value in page.value.into_iter() {
            sender.send(value).unwrap();
        }
        match page.next_link {
            Some(next) => {
                link = next;
            },
            None => {
                break;
            }
        }
    }
}

pub fn sync_drive_items<DriveItem: 'static>(
    client: &reqwest::Client,
    drive_id: &str,
    handler: &mut impl DriveItemHandler<DriveItem>
) -> Result<(), Box<Error>>
where DriveItem: Send + serde::de::DeserializeOwned
{
    let link = format!("https://graph.microsoft.com/v1.0/me/drives/{}/root/delta", drive_id);
    let (sender, receiver) = mpsc::channel::<DriveItem>();
    let client = client.clone();
    let t = std::thread::spawn(move || {
        fetch_items(&client, link, sender)
    });
    handler.tick();
    loop {
        match receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(item) => {
                handler.handle(item);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                handler.tick();
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break;
            }
        }
    }
    t.join().map_err(|any| string_error::into_err(format!("{:?}", any)))?;
    Ok(())
}