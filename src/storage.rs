use std::io::Write;
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

    pub fn save(&self, state: &T) -> eyre::Result<()>
    where
        T: serde::ser::Serialize,
    {
        if let Some(path) = &self.path {
            let file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
            let mut writer = std::io::BufWriter::new(file);
            serde_cbor::to_writer(&mut writer, &state)?;
            writer.flush()?;
            writer.into_inner()?.persist(path)?;
        }
        Ok(())
    }
}
