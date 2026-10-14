//! Writing one key of `config.toml` the way a person would, for the Settings window.
//!
//! The file keeps every comment, blank line and key order. The default file holds every key
//! commented out (`# key = default`), so a change uncomments that line in place with the new
//! value, and going back to the built-in default comments it out again: a key left at its
//! default stays out of the file's live values, and a later default still reaches the user.
//! A key the user wrote elsewhere (an inline table, a dotted key, a line with its own comment)
//! is changed where it is with `toml_edit`, which keeps its decoration. Every result is
//! parsed and validated before it is returned; a file that does not parse is never written.

use std::path::Path;

use super::schema::{self, KeyDef};
use super::{Config, locate};

/// Why a key could not be set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditError(pub String);

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EditError {}

/// `text` with `path` (`section.key`) set to the TOML literal `literal` (`true`, `1.5`,
/// `"move"`). Returns the new text; unchanged text when nothing had to change.
pub fn set(text: &str, path: &str, literal: &str) -> Result<String, EditError> {
    let d = schema::find(path).ok_or_else(|| EditError(format!("{path} is not a config key")))?;
    let wanted = literal_value(literal).ok_or_else(|| EditError(format!("{literal} is not a TOML value")))?;
    let doc: toml_edit::DocumentMut =
        text.parse().map_err(|e: toml_edit::TomlError| EditError(format!("config.toml does not parse: {e}")))?;
    let is_default = literal_value(d.default).is_some_and(|v| same(&v, &wanted));
    let live = doc.get(d.section).and_then(|t| t.get(d.key)).is_some();
    let out = match (live, is_default) {
        (false, true) => return Ok(text.to_string()),
        (false, false) => uncomment_or_insert(text, d, literal).unwrap_or_else(|| with_toml_edit(doc, d, literal)),
        (true, true) => comment_out(text, d).unwrap_or_else(|| with_toml_edit(doc, d, literal)),
        (true, false) => replace_plain_line(text, d, literal).unwrap_or_else(|| with_toml_edit(doc, d, literal)),
    };
    check(&out, d, &wanted)?;
    Ok(out)
}

/// Sets `path` in the file at `file` (creating it with the default file when missing) and
/// writes it back atomically (a temporary file renamed over it, its permissions kept). A
/// symlinked file (dotfiles) is written through to its target; the link stays. Returns
/// whether the file changed.
pub fn set_in_file(file: &Path, path: &str, literal: &str) -> Result<bool, EditError> {
    let linked = std::fs::symlink_metadata(file).is_ok_and(|m| m.file_type().is_symlink());
    let target = if linked { std::fs::canonicalize(file).ok() } else { None };
    let file = target.as_deref().unwrap_or(file);
    let io = |e: std::io::Error| EditError(format!("{}: {e}", file.display()));
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(dir) = file.parent() {
                crate::private_files::ensure_dir(dir).map_err(io)?;
            }
            super::default_file()
        }
        Err(e) => return Err(io(e)),
    };
    let new = set(&text, path, literal)?;
    if new == text && file.exists() {
        return Ok(false);
    }
    let tmp = file.with_extension("toml.saving");
    // Owner-only until it takes the file's own mode: never readable by others in between.
    crate::private_files::write(&tmp, new.as_bytes()).map_err(io)?;
    if let Ok(meta) = std::fs::metadata(file) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, file).map_err(io)?;
    Ok(true)
}

/// A TOML literal as a value (`toml::Value` compares floats and integers by kind).
fn literal_value(literal: &str) -> Option<toml::Value> {
    toml::from_str::<toml::Table>(&format!("v = {literal}")).ok().and_then(|mut t| t.remove("v"))
}

/// Equal values, a whole number and a float of the same value included.
fn same(a: &toml::Value, b: &toml::Value) -> bool {
    match (a.as_float(), b.as_integer(), a.as_integer(), b.as_float()) {
        (Some(x), Some(y), _, _) | (_, _, Some(y), Some(x)) => x == y as f64,
        _ => a == b,
    }
}

/// The result parses, validates, and holds the wanted value at the key.
fn check(text: &str, d: &KeyDef, wanted: &toml::Value) -> Result<(), EditError> {
    Config::parse(text).map_err(|e| EditError(e.0))?;
    let table: toml::Table = toml::from_str(text).map_err(|e| EditError(e.to_string()))?;
    let got = table.get(d.section).and_then(|s| s.get(d.key));
    let default = literal_value(d.default);
    let ok = match got {
        Some(v) => same(v, wanted),
        None => default.as_ref().is_some_and(|v| same(v, wanted)),
    };
    if ok { Ok(()) } else { Err(EditError(format!("{} did not take the value {wanted}", d.path()))) }
}

/// The line range `[start, end)` of `[section]`'s body (after its header), if it has one.
fn section_body(lines: &[&str], section: &str) -> Option<(usize, usize)> {
    let header = |l: &str| {
        let t = l.trim();
        t.strip_prefix('[').filter(|r| !r.starts_with('[')).map(|r| r.split(']').next().unwrap_or("").trim().to_string())
    };
    let is_table_start = |l: &str| l.trim_start().starts_with('[');
    let start = lines.iter().position(|l| header(l).as_deref() == Some(section))? + 1;
    let end = lines[start..].iter().position(|l| is_table_start(l)).map_or(lines.len(), |n| start + n);
    Some((start, end))
}

/// Joins lines back, keeping the text's final newline.
fn join(lines: &[String], original: &str) -> String {
    let mut out = lines.join("\n");
    if original.ends_with('\n') || original.is_empty() {
        out.push('\n');
    }
    out
}

/// A commented `# key = …` line of the section becomes `key = literal`; without one the key
/// goes right after the section's header, and without the section a new one ends the file.
fn uncomment_or_insert(text: &str, d: &KeyDef, literal: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let live = format!("{} = {literal}", d.key);
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    match section_body(&lines, d.section) {
        Some((start, end)) => {
            let commented = (start..end).find(|&i| {
                lines[i]
                    .trim_start()
                    .strip_prefix('#')
                    .map(str::trim_start)
                    .and_then(|r| r.strip_prefix(d.key))
                    .is_some_and(|r| r.trim_start().starts_with('='))
            });
            match commented {
                Some(i) => out[i] = live,
                None => out.insert(start, live),
            }
        }
        None => {
            // A section written another way (an inline table, dotted keys) is toml_edit's.
            let doc: toml_edit::DocumentMut = text.parse().ok()?;
            if doc.get(d.section).is_some() {
                return None;
            }
            if out.last().is_some_and(|l| !l.trim().is_empty()) {
                out.push(String::new());
            }
            out.push(format!("[{}]", d.section));
            out.push(live);
        }
    }
    Some(join(&out, text))
}

/// The plain live line of the key (no trailing comment) back to `# key = default`.
fn comment_out(text: &str, d: &KeyDef) -> Option<String> {
    let i = plain_line(text, d)?;
    let mut out: Vec<String> = text.lines().map(str::to_string).collect();
    out[i] = format!("# {} = {}", d.key, d.default);
    Some(join(&out, text))
}

/// The plain live line of the key with a new value.
fn replace_plain_line(text: &str, d: &KeyDef, literal: &str) -> Option<String> {
    let i = plain_line(text, d)?;
    let mut out: Vec<String> = text.lines().map(str::to_string).collect();
    out[i] = format!("{} = {literal}", d.key);
    Some(join(&out, text))
}

/// The 0-based index of the key's `key = value` line in its section, when the line holds
/// nothing else (no comment after the value, no value spanning lines).
fn plain_line(text: &str, d: &KeyDef) -> Option<usize> {
    let i = locate(text, d.section, d.key)? - 1;
    let line = text.lines().nth(i)?;
    let value = line.split_once('=')?.1.trim();
    let single: Option<toml::Value> = literal_value(value);
    (single.is_some() && !value.contains('#') && !value.starts_with('[') && !value.starts_with('{')).then_some(i)
}

/// Sets the key with `toml_edit`, keeping the old value's decoration (its comments).
fn with_toml_edit(mut doc: toml_edit::DocumentMut, d: &KeyDef, literal: &str) -> String {
    let Ok(new) = literal.parse::<toml_edit::Value>() else { return doc.to_string() };
    let table = doc.entry(d.section).or_insert_with(toml_edit::table);
    if let Some(t) = table.as_table_like_mut() {
        match t.get_mut(d.key).and_then(|i| i.as_value_mut()) {
            Some(old) => {
                let decor = old.decor().clone();
                *old = new;
                *old.decor_mut() = decor;
            }
            None => {
                t.insert(d.key, toml_edit::Item::Value(new));
            }
        }
    }
    doc.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::default_file;

    fn get(text: &str, path: &str) -> Option<toml::Value> {
        let (s, k) = path.split_once('.')?;
        toml::from_str::<toml::Table>(text).ok()?.get(s)?.get(k).cloned()
    }

    #[test]
    fn a_change_uncomments_the_default_files_line_in_place() {
        let before = default_file();
        let after = set(&before, "strip.merge_on_drop", "false").expect("sets");
        assert_eq!(get(&after, "strip.merge_on_drop"), Some(toml::Value::Boolean(false)));
        assert_eq!(after.lines().count(), before.lines().count(), "one line changed, none added");
        let changed: Vec<_> = before.lines().zip(after.lines()).filter(|(a, b)| a != b).collect();
        assert_eq!(changed, vec![("# merge_on_drop = true", "merge_on_drop = false")]);
        assert!(!Config::parse(&after).expect("parses").config.style.merge_on_drop);
    }

    #[test]
    fn going_back_to_the_default_comments_the_line_out_again() {
        let before = default_file();
        let off = set(&before, "app.launch_at_login", "false").expect("sets");
        let back = set(&off, "app.launch_at_login", "true").expect("sets");
        assert_eq!(back, before, "the default file again, byte for byte");
        assert!(crate::config::is_legacy_default(&back) || back == default_file());
    }

    #[test]
    fn a_default_value_never_adds_a_line() {
        let before = default_file();
        assert_eq!(set(&before, "animation.speed", "1.0").expect("sets"), before);
        assert_eq!(set(&before, "animation.speed", "1").expect("sets"), before, "1 and 1.0 are the same speed");
    }

    #[test]
    fn a_live_value_changes_where_it_is_and_keeps_comments() {
        let text = "# mine\n[animation]\n# faster please\nspeed = 2.0\n\n[strip]\nwidth = 220.0 # wide\n";
        let out = set(text, "animation.speed", "1.5").expect("sets");
        assert_eq!(out, "# mine\n[animation]\n# faster please\nspeed = 1.5\n\n[strip]\nwidth = 220.0 # wide\n");
        let out = set(text, "strip.width", "240.0").expect("sets");
        assert!(out.contains("width = 240.0 # wide"), "the trailing comment stays: {out}");
    }

    #[test]
    fn a_missing_key_goes_under_its_section_and_a_missing_section_is_added() {
        let text = "[strip]\n# my note\nwidth = 220.0\n";
        let out = set(text, "strip.merge_on_drop", "false").expect("sets");
        assert_eq!(out, "[strip]\nmerge_on_drop = false\n# my note\nwidth = 220.0\n");
        let out = set(text, "app.show_menu_bar_icon", "false").expect("sets");
        assert_eq!(out, "[strip]\n# my note\nwidth = 220.0\n\n[app]\nshow_menu_bar_icon = false\n");
        let out = set("", "app.check_for_updates", "false").expect("sets");
        assert_eq!(get(&out, "app.check_for_updates"), Some(toml::Value::Boolean(false)));
    }

    #[test]
    fn keys_written_another_way_are_changed_there() {
        let text = "app = { show_menu_bar_icon = true }\n";
        let out = set(text, "app.show_menu_bar_icon", "false").expect("sets");
        assert_eq!(get(&out, "app.show_menu_bar_icon"), Some(toml::Value::Boolean(false)));
        let text = "[windows]\nfit = \"move\" # keep clear\n";
        let out = set(text, "windows.fit", "\"off\"").expect("sets");
        assert!(out.contains("fit = \"off\" # keep clear"), "{out}");
    }

    #[test]
    fn rules_after_the_section_are_left_alone() {
        let text = "[defaults]\n# action = \"native\"\n\n[[rule]]\napp = \"com.example.chat\"\naction = \"new\"\n";
        let out = set(text, "defaults.placement", "\"center\"").expect("sets");
        assert!(out.ends_with("[[rule]]\napp = \"com.example.chat\"\naction = \"new\"\n"), "{out}");
        assert_eq!(Config::parse(&out).expect("parses").config.rules.rules.len(), 1);
    }

    #[test]
    fn bad_values_and_broken_files_are_refused() {
        assert!(set(&default_file(), "strip.nope", "true").is_err());
        assert!(set(&default_file(), "windows.fit", "\"sideways\"").is_err());
        assert!(set(&default_file(), "strip.merge_on_drop", "maybe").is_err());
        assert!(set("[strip\nbroken", "strip.merge_on_drop", "false").is_err());
    }

    #[test]
    fn files_are_written_atomically_and_created_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("sub").join("config.toml");
        assert!(set_in_file(&file, "app.show_menu_bar_icon", "false").expect("writes"));
        let text = std::fs::read_to_string(&file).expect("reads");
        assert!(text.starts_with("# Stage Manager configuration."));
        assert!(!Config::parse(&text).expect("parses").config.app.show_menu_bar_icon);
        assert!(!set_in_file(&file, "app.show_menu_bar_icon", "false").expect("writes"), "no change, no write");
        assert!(!file.with_extension("toml.saving").exists());
    }

    /// R8 L1: a dotfiles symlink keeps pointing at the user's file, which takes the change.
    #[test]
    fn a_symlinked_file_is_written_through_and_the_link_stays() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("dotfiles-config.toml");
        std::fs::write(&real, default_file()).expect("writes");
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &link).expect("links");
        assert!(set_in_file(&link, "strip.merge_on_drop", "false").expect("writes"));
        assert!(std::fs::symlink_metadata(&link).expect("meta").file_type().is_symlink(), "still a link");
        let text = std::fs::read_to_string(&real).expect("reads");
        assert!(!Config::parse(&text).expect("parses").config.style.merge_on_drop, "the target took the change");
    }
}
