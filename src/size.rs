use std::collections::{BTreeMap, HashMap};

use crate::item::{Item, ItemType};

#[derive(PartialEq, Eq, Hash)]
pub(crate) enum ItemHash {
    Sha1(String),
    QuickXor(String),
}

fn ignore_path(dirname: &str, basename: &str) -> bool {
    // SVN repo files may be duplicated in the .svn directory. Don't match these,
    // as they are part of the SVN repo format, and should not be modified
    // individually.
    basename.ends_with(".svn-base") && dirname.contains("/.svn/pristine/")
}

pub(crate) fn bucket_by_size(
    names_by_hash: &HashMap<String, Item>,
) -> (u32, u32, BTreeMap<u64, HashMap<ItemHash, Vec<String>>>) {
    let mut names_by_hash_by_size = BTreeMap::<u64, HashMap<ItemHash, Vec<String>>>::new();
    let mut file_count = 0;
    let mut folder_count = 0;
    let bar = indicatif::ProgressBar::new(names_by_hash.len() as u64);
    bar.set_style(
        indicatif::ProgressStyle::default_bar()
            .template("Analyzing duplicates: [{elapsed_precise}] {wide_bar} {percent}%")
            .progress_chars("#>-"),
    );
    bar.tick();
    for item in names_by_hash.values() {
        bar.inc(1);
        match &item.item_type {
            ItemType::File { hashes } => {
                file_count += 1;
                let dirname = match item.parent.path {
                    None => {
                        // deleted parent
                        continue;
                    }
                    Some(ref path) => path.trim_start_matches("/drive/root:/"),
                };
                if ignore_path(dirname, &item.name) {
                    continue;
                }
                let hash = match hashes {
                    Some(hashes) => match item.parent.drive_type.as_ref() {
                        "personal" => match hashes.sha {
                            Some(ref sha) => ItemHash::Sha1(sha.clone()),
                            None => {
                                eprintln!("Ignoring item due to missing sha1 hash: {:?}\n", item);
                                continue;
                            }
                        },
                        "business" | "documentLibrary" => match hashes.xor {
                            Some(ref xor) => ItemHash::QuickXor(xor.clone()),
                            None => {
                                eprintln!(
                                    "Ignoring item due to missing quickXor hash: {:?}\n",
                                    item
                                );
                                continue;
                            }
                        },
                        _ => {
                            eprintln!("Ignoring item due to unknown drive_type: {:?}\n", item);
                            continue;
                        }
                    },
                    None => {
                        // Files with the "application/msonenote" MIME Type do not have a SHA.
                        continue;
                    }
                };
                let names_by_hash = names_by_hash_by_size
                    .entry(item.size)
                    .or_insert_with(HashMap::<ItemHash, Vec<String>>::new);
                // allocating the key only on insert is messy - we could use raw_entry here,
                // or maybe entry_ref() will exist one day - for now, always allocate
                let v = names_by_hash.entry(hash).or_insert_with(Vec::<String>::new);
                let name = format!("{}/{}", dirname, item.name);
                v.push(name);
            }
            ItemType::Folder {} | ItemType::Package {} => {
                folder_count += 1;
            }
        }
    }
    bar.finish_and_clear();
    (file_count, folder_count, names_by_hash_by_size)
}

pub(crate) fn size_as_string(value: u64) -> String {
    if value < 32 * 1024 {
        format!("{} bytes", value)
    } else {
        let mib = value as f32 / 1024.0 / 1024.0;
        if mib < 1000.0 {
            format!("{:.3} MiB", mib)
        } else {
            let gib = mib / 1024.0;
            format!("{:.3} GiB", gib)
        }
    }
}
