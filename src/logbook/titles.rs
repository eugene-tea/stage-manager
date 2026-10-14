//! Window titles in the log: kept out unless `[log] titles` is on.
//!
//! Titles name documents, mail subjects and chats, and the log keeps two weeks of them. By
//! default a line carries a short hash of the title instead ([`title`]): the same title
//! always gives the same hash (in every process and on every day), so a line can still be
//! followed across a retitle or a relaunch without saying what the window shows. An empty
//! title stays visibly empty. Problem marks hash the `title` fields of their state dumps the
//! same way ([`redact_titles`]).

use serde_json::Value;

/// A title as a log line shows it: quoted as it is when `keep`, else `#` and eight hex
/// digits of its hash (`""` for an empty title either way).
pub fn title(title: &str, keep: bool) -> String {
    if keep || title.is_empty() { format!("{title:?}") } else { format!("#{:08x}", fnv1a(title)) }
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

/// 32-bit FNV-1a: stable across processes, builds and platforms (std's hasher is not).
fn fnv1a(text: &str) -> u32 {
    text.bytes().fold(0x811c_9dc5_u32, |h, b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193))
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
        // FNV-1a's published test vector: the hash never changes between builds.
        assert_eq!(fnv1a("a"), 0xe40c_292c);
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
