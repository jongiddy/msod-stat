use rand::{thread_rng, Rng};
use std::marker::PhantomData;

pub struct Storage<T> {
    path: Option<std::path::PathBuf>,
    _phantom: PhantomData<fn(T) -> T>, // Same type must be saved and loaded at this path
}

impl<T> Storage<T> {
    pub fn new(path: Option<std::path::PathBuf>) -> Storage<T> {
        Storage {
            path,
            _phantom: PhantomData,
        }
    }

    pub fn load(&self) -> Option<T>
    where
        T: serde::de::DeserializeOwned,
    {
        if let Some(path) = &self.path {
            match std::fs::File::open(path) {
                Ok(file) => {
                    let reader = std::io::BufReader::new(file);
                    match serde_cbor::from_reader(reader) {
                        Ok(state) => {
                            return Some(state);
                        }
                        Err(error) => {
                            // storage file corrupted
                            eprintln!("{}\n", error);
                        }
                    }
                }
                Err(_) => {
                    // file does not exist, don't display an error for this common state.
                }
            }
        }
        None
    }

    pub fn save(&self, state: &T)
    where
        T: serde::ser::Serialize,
    {
        if let Some(path) = &self.path {
            let mut rng = thread_rng();
            let int = rng.gen_range(1000..10000);
            let mut tmp_path = path.to_path_buf();
            assert!(tmp_path.set_extension(int.to_string()));
            match std::fs::File::create(&tmp_path) {
                Ok(file) => {
                    let result = {
                        let mut writer = std::io::BufWriter::new(file);
                        serde_cbor::to_writer(&mut writer, &state)
                    };
                    if let Err(error) = result {
                        eprintln!("{}\n", error);
                    } else {
                        if let Err(error) = std::fs::rename(&tmp_path, path) {
                            eprintln!("{}\n", error);
                        } else {
                            return;
                        }
                    }
                    // tmp_path was created but not renamed.
                    if let Err(error) = std::fs::remove_file(&tmp_path) {
                        eprintln!("{}\n", error);
                    }
                }
                Err(error) => {
                    eprintln!("{}\n", error);
                }
            }
        }
    }
}
