use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::mpsc;
use std::time::Duration;
use reqwest::StatusCode;
use serde_json::{json, Value};


pub trait ProgressIndicator {
    fn update(&self);

    fn insert(&mut self, item: &Value);

    fn delete(&mut self, prev: &Value);
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

pub fn start_fetcher(client: &reqwest::Client, drive_id: &str)
    -> (std::thread::JoinHandle<()>, mpsc::Receiver<Option<Value>>)
{
    let mut link = format!("https://graph.microsoft.com/v1.0/me/drives/{}/root/delta", drive_id);
    let (sender, receiver) = mpsc::channel::<Option<Value>>();
    let client = client.clone();
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
                    break;
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

pub fn get_items(receiver: mpsc::Receiver<Option<Value>>, progress: &mut impl ProgressIndicator) -> HashMap<String, Value> {
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

pub fn get_id_item_map(client: &reqwest::Client, drive_id: &str, progress: &mut impl ProgressIndicator)
    -> HashMap<String, Value>
{
        let (thread, receiver) = start_fetcher(client, drive_id);
        let id_map = get_items(receiver, progress);
        thread.join().unwrap();
        id_map
}