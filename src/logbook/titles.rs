//! Window titles in the log: kept out unless `[log] titles` is on.
//!
//! Titles name documents, mail subjects and chats, and the log keeps two weeks of them. By
//! default a line carries a short hash of the title instead ([`title`]): the same title
//! gives the same hash on this Mac (in every process and on every day), so a line can still
//! be followed across a retitle or a relaunch without saying what the window shows. An empty
//! title stays visibly empty. Problem marks hash the `title` fields of their state dumps the
//! same way ([`redact_titles`]).
//!
//! The hash is keyed (SipHash-2-4, [`siphash`]) with a random key made once per install and
//! kept owner-only in the config folder ([`load_or_create_key`], [`KEY_FILE`]): without the
//! key, a guessed title cannot be confirmed against the log alone. Until the app hands one
//! over ([`set_key`]) a key for this process alone is used.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::OnceLock;

use serde_json::Value;

/// A title hash's key: 128 bits.
pub type Key = [u8; 16];

/// The key's file in the config folder: 32 hex digits, owner-only.
pub const KEY_FILE: &str = "title-key";

static KEY: OnceLock<Key> = OnceLock::new();

/// Sets the key titles are hashed with in this process (the first call wins; false when
/// one was already in use).
pub fn set_key(key: Key) -> bool {
    KEY.set(key).is_ok()
}

/// The key in use: the one set, else a random one for this process alone (hashes then hold
/// only within it).
fn key() -> &'static Key {
    KEY.get_or_init(|| {
        use std::hash::{BuildHasher, RandomState};
        let state = RandomState::new();
        let (a, b) = (state.hash_one(1u8), state.hash_one(2u8));
        let mut key = [0; 16];
        key[..8].copy_from_slice(&a.to_le_bytes());
        key[8..].copy_from_slice(&b.to_le_bytes());
        key
    })
}

/// The key kept in `dir` ([`KEY_FILE`]); made with `random` and written owner-only (`0600`)
/// when there is none or it is unreadable. Two processes making it at once agree: the file is
/// created only when absent, and the one that loses reads the winner's.
pub fn load_or_create_key(dir: &Path, random: impl Fn() -> Key) -> io::Result<Key> {
    let path = dir.join(KEY_FILE);
    for _ in 0..3 {
        match read_key(&path) {
            Ok(Some(key)) => return Ok(key),
            Ok(None) => {
                // Malformed (cut short, edited): replaced.
                let _ = std::fs::remove_file(&path);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        crate::private_files::ensure_dir(dir)?;
        let key = random();
        let created = OpenOptions::new().write(true).create_new(true).mode(crate::private_files::FILE_MODE).open(&path);
        match created {
            Ok(mut file) => {
                file.write_all(format!("{}\n", hex(&key)).as_bytes())?;
                file.sync_all()?;
                return Ok(key);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("the title key file keeps changing"))
}

/// The key in `path`; `None` when the file is not one (`Err` when it cannot be read).
fn read_key(path: &Path) -> io::Result<Option<Key>> {
    let mut text = String::new();
    std::fs::File::open(path)?.take(256).read_to_string(&mut text)?;
    let text = text.trim();
    if text.len() != 32 {
        return Ok(None);
    }
    let mut key = [0u8; 16];
    for (i, byte) in key.iter_mut().enumerate() {
        match u8::from_str_radix(text.get(i * 2..i * 2 + 2).unwrap_or(""), 16) {
            Ok(b) => *byte = b,
            Err(_) => return Ok(None),
        }
    }
    Ok(Some(key))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A title as a log line shows it: quoted as it is when `keep`, else `#` and eight hex
/// digits of its hash (`""` for an empty title either way).
pub fn title(title: &str, keep: bool) -> String {
    if keep || title.is_empty() { format!("{title:?}") } else { format!("#{:08x}", hash(key(), title)) }
}

/// Replaces the string value of every `"title"` key in `v` (at any depth) with its hash, as
/// [`title`] writes it without `keep`.
pub fn redact_titles(v: &mut Value) {
    match v {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                match value {
                    Value::String(s) if key == "title" => {
                        if !s.is_empty() {
                            *s = title(s, false);
                        }
                    }
                    other => redact_titles(other),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_titles),
        _ => {}
    }
}

/// The file name of a picture (the wallpaper) as a log line shows it: Apple's own pictures
/// (under `/System/Library/` or `/Library/Desktop Pictures/`) and any with `keep` by name;
/// the user's own as a hash of the name plus its extension (`#1a2b3c4d.heic`), since the name
/// of a personal photo says as much as a window title. Never the folders.
pub fn picture_name(path: &str, keep: bool) -> String {
    let name = path.rsplit('/').next().unwrap_or_default();
    let apple = path.starts_with("/System/Library/") || path.starts_with("/Library/Desktop Pictures/");
    if keep || apple || name.is_empty() {
        return name.to_string();
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{}.{ext}", title(stem, false)),
        _ => title(name, false),
    }
}

/// A title's hash under `key`: the low 32 bits of its SipHash-2-4.
fn hash(key: &Key, text: &str) -> u32 {
    siphash(key, text.as_bytes()) as u32
}

/// SipHash-2-4 (Aumasson and Bernstein): a keyed hash, stable across processes, builds and
/// platforms (std's hashers are not promised to be), that tells nothing about its input
/// without the key.
fn siphash(key: &Key, data: &[u8]) -> u64 {
    let word = |b: &[u8]| u64::from_le_bytes(b.try_into().unwrap_or([0; 8]));
    let (k0, k1) = (word(&key[..8]), word(&key[8..]));
    let mut v = [k0 ^ 0x736f_6d65_7073_6575, k1 ^ 0x646f_7261_6e64_6f6d, k0 ^ 0x6c79_6765_6e65_7261, k1 ^ 0x7465_6462_7974_6573];
    fn round(v: &mut [u64; 4]) {
        v[0] = v[0].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(13) ^ v[0];
        v[0] = v[0].rotate_left(32);
        v[2] = v[2].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(16) ^ v[2];
        v[0] = v[0].wrapping_add(v[3]);
        v[3] = v[3].rotate_left(21) ^ v[0];
        v[2] = v[2].wrapping_add(v[1]);
        v[1] = v[1].rotate_left(17) ^ v[2];
        v[2] = v[2].rotate_left(32);
    }
    fn compress(m: u64, v: &mut [u64; 4]) {
        v[3] ^= m;
        round(v);
        round(v);
        v[0] ^= m;
    }
    let (chunks, tail) = data.as_chunks::<8>();
    for chunk in chunks {
        compress(u64::from_le_bytes(*chunk), &mut v);
    }
    let mut last = [0u8; 8];
    last[..tail.len()].copy_from_slice(tail);
    last[7] = data.len() as u8;
    compress(u64::from_le_bytes(last), &mut v);
    v[2] ^= 0xff;
    for _ in 0..4 {
        round(&mut v);
    }
    v[0] ^ v[1] ^ v[2] ^ v[3]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_title_is_hashed_unless_kept() {
        let hashed = title("Quarterly plan.numbers", false);
        assert!(hashed.starts_with('#') && hashed.len() == 9, "{hashed}");
        assert!(!hashed.contains("plan"));
        assert_eq!(hashed, title("Quarterly plan.numbers", false), "stable");
        assert_ne!(hashed, title("Quarterly plan.numbers ", false));
        assert_eq!(title("Quarterly plan.numbers", true), "\"Quarterly plan.numbers\"");
        assert_eq!(title("", false), "\"\"", "an empty title stays visibly empty");
    }

    #[test]
    fn siphash_matches_the_reference_vectors() {
        // The SipHash paper's key 00 01 … 0f; inputs 00 01 … (n-1) (vectors.h).
        let key: Key = std::array::from_fn(|i| i as u8);
        let data: Vec<u8> = (0..64).map(|i| i as u8).collect();
        assert_eq!(siphash(&key, &data[..0]), 0x726f_db47_dd0e_0e31);
        assert_eq!(siphash(&key, &data[..1]), 0x74f8_39c5_93dc_67fd);
        assert_eq!(siphash(&key, &data[..8]), 0x93f5_f579_9a93_2462);
        assert_eq!(siphash(&key, &data[..15]), 0xa129_ca61_49be_45e5);
        assert_eq!(siphash(&key, &data[..63]), 0x958a_324c_eb06_4572);
    }

    #[test]
    fn the_key_decides_the_hash() {
        let (a, b): (Key, Key) = ([1; 16], [2; 16]);
        assert_eq!(hash(&a, "Quarterly plan.numbers"), hash(&a, "Quarterly plan.numbers"));
        assert_ne!(hash(&a, "Quarterly plan.numbers"), hash(&b, "Quarterly plan.numbers"), "another install, another hash");
    }

    #[test]
    fn the_key_is_made_once_owner_only_and_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("config");
        let first = load_or_create_key(&config, || [7; 16]).expect("made");
        assert_eq!(first, [7; 16]);
        let path = config.join(KEY_FILE);
        assert_eq!(crate::private_files::mode(&path).expect("mode"), 0o600);
        let never = || -> Key { panic!("a key on file is never made again") };
        assert_eq!(load_or_create_key(&config, never).expect("read"), first, "the next process reads it");
        // A broken file is replaced.
        std::fs::write(&path, "not a key\n").expect("writes");
        assert_eq!(load_or_create_key(&config, || [9; 16]).expect("remade"), [9; 16]);
        assert_eq!(load_or_create_key(&config, never).expect("read"), [9; 16]);
        assert_eq!(crate::private_files::mode(&path).expect("mode"), 0o600);
    }

    #[test]
    fn a_personal_picture_keeps_only_its_extension() {
        let mine = picture_name("/Users/someone/Pictures/Kids at the lake.heic", false);
        assert_eq!(mine, format!("{}.heic", title("Kids at the lake", false)));
        assert!(!mine.contains("lake") && !mine.contains("someone"));
        assert_eq!(picture_name("/Users/someone/Pictures/Kids at the lake.heic", true), "Kids at the lake.heic");
        assert_eq!(picture_name("/System/Library/Desktop Pictures/Sonoma.heic", false), "Sonoma.heic");
        assert_eq!(picture_name("/Library/Desktop Pictures/Solid Colors/Space Gray.png", false), "Space Gray.png");
        assert_eq!(picture_name("/Users/someone/.hidden", false), title(".hidden", false));
        assert_eq!(picture_name("", false), "");
    }

    #[test]
    fn redaction_hashes_every_title_field_and_nothing_else() {
        let mut v = json!({
            "ok": true,
            "windows": [
                { "id": 7, "title": "Inbox — 3 unread", "bundle_id": "com.example.mail" },
                { "id": 8, "title": "" },
            ],
            "stages": [{ "name": "Work", "nested": { "title": "Draft" } }],
            "title_count": 2,
        });
        redact_titles(&mut v);
        assert_eq!(v["windows"][0]["title"], json!(title("Inbox — 3 unread", false)));
        assert_eq!(v["windows"][1]["title"], json!(""));
        assert_eq!(v["stages"][0]["nested"]["title"], json!(title("Draft", false)));
        assert_eq!(v["stages"][0]["name"], json!("Work"), "stage names are the user's own words, kept");
        assert_eq!(v["windows"][0]["bundle_id"], json!("com.example.mail"));
        assert_eq!(v["title_count"], json!(2));
        assert!(!v.to_string().contains("Inbox") && !v.to_string().contains("Draft"));
    }
}
