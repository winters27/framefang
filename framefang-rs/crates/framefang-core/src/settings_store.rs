//! `settings.json` on disk is the one truth; every change is a patch.
//!
//! Several things hold a copy of the settings: the tray, the Settings window,
//! an open capture editor. Saving a whole copy wrote back whatever that copy
//! had gone stale on (the editor once undid a Settings change, a tray save
//! once dropped a mode picked a second earlier). So nothing saves a whole
//! copy any more. Each holder sends only what it changed, as a [`Patch`],
//! and [`update`] applies it to the file as it is on disk right now, then
//! writes the result. Holders then take the result as their new copy.
//!
//! A patch holds whole top-level keys, except that a key holding a map
//! (`ToolStyles`) holds only the entries that changed, each entry whole. Two
//! holders changing different tools' styles therefore never undo each other.
//!
//! A file that doesn't parse (a hand edit in progress) is never written
//! over: [`update`] refuses and the caller keeps the patch for later.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Map, Value};

use crate::settings::AppSettings;

/// Keys and values that differ, ready to apply to any copy.
pub type Patch = Map<String, Value>;

/// What a file on disk looked like when it was last read or written, so a
/// change by anything else (a hand edit, a restored backup with an older
/// date) is noticed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub modified: Option<SystemTime>,
    pub len: u64,
    pub hash: u64,
}

#[derive(Debug)]
pub enum StoreError {
    /// The file is there but isn't valid settings; the message names the
    /// line. Nothing was written.
    Unparsable(String),
    Io(io::Error),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Unparsable(e) => write!(f, "settings.json couldn't be read ({e})"),
            StoreError::Io(e) => write!(f, "settings.json couldn't be saved ({e})"),
        }
    }
}

fn to_value(s: &AppSettings) -> Map<String, Value> {
    match serde_json::to_value(s) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// What changed from `base` to `new`.
pub fn diff(base: &AppSettings, new: &AppSettings) -> Patch {
    let (a, b) = (to_value(base), to_value(new));
    let mut out = Patch::new();
    for (key, nv) in &b {
        let ov = a.get(key);
        if ov == Some(nv) { continue; }
        match (ov, nv) {
            (Some(Value::Object(om)), Value::Object(nm)) => {
                let mut entries = Map::new();
                for (k, v) in nm {
                    if om.get(k) != Some(v) { entries.insert(k.clone(), v.clone()); }
                }
                if !entries.is_empty() { out.insert(key.clone(), Value::Object(entries)); }
            }
            _ => { out.insert(key.clone(), nv.clone()); }
        }
    }
    out
}

/// Every key of `s`: a patch that makes any file exactly `s` (an import
/// waiting to be written).
pub fn whole(s: &AppSettings) -> Patch { to_value(s) }

/// Merge two patches; `later` wins key by key (and entry by entry).
pub fn merge(into: &mut Patch, later: Patch) {
    for (k, v) in later {
        match (into.get_mut(&k), v) {
            (Some(Value::Object(a)), Value::Object(b)) => { for (ek, ev) in b { a.insert(ek, ev); } }
            (_, v) => { into.insert(k, v); }
        }
    }
}

/// `patch` applied to `base`, key by key. A key whose value would not make
/// valid settings is left out and named in the second value.
pub fn apply(base: &AppSettings, patch: &Patch) -> (AppSettings, Vec<String>) {
    let mut cur = to_value(base);
    let mut refused = Vec::new();
    for (key, v) in patch {
        let mut candidate = cur.clone();
        match (candidate.get_mut(key), v) {
            (Some(Value::Object(a)), Value::Object(b)) => { for (ek, ev) in b { a.insert(ek.clone(), ev.clone()); } }
            _ => { candidate.insert(key.clone(), v.clone()); }
        }
        if serde_json::from_value::<AppSettings>(Value::Object(candidate.clone())).is_ok() {
            cur = candidate;
        } else {
            refused.push(key.clone());
        }
    }
    let mut out: AppSettings = serde_json::from_value(Value::Object(cur)).unwrap_or_else(|_| base.clone());
    out.clamp();
    (out, refused)
}

fn hash(bytes: &[u8]) -> u64 {
    // FNV-1a: stable across runs, enough to tell two versions apart.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes { h ^= *b as u64; h = h.wrapping_mul(0x0000_0100_0000_01b3); }
    h
}

/// The file's stamp now, or `None` when it doesn't exist.
pub fn stamp_of(path: &Path) -> Option<Stamp> {
    let bytes = fs::read(path).ok()?;
    let modified = fs::metadata(path).ok().and_then(|m| m.modified().ok());
    Some(Stamp { modified, len: bytes.len() as u64, hash: hash(&bytes) })
}

/// The settings in `path` and their stamp. A missing file is the defaults
/// (stamp `None`); a file that doesn't parse is an error naming the line.
pub fn read(path: &Path) -> Result<(AppSettings, Option<Stamp>), StoreError> {
    match fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let s = AppSettings::parse(&text).map_err(StoreError::Unparsable)?;
            let modified = fs::metadata(path).ok().and_then(|m| m.modified().ok());
            Ok((s, Some(Stamp { modified, len: bytes.len() as u64, hash: hash(&bytes) })))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok((AppSettings::default(), None)),
        Err(e) => Err(StoreError::Io(e)),
    }
}

fn write(path: &Path, s: &AppSettings) -> Result<Stamp, StoreError> {
    if let Some(dir) = path.parent() { fs::create_dir_all(dir).map_err(StoreError::Io)?; }
    let json = serde_json::to_string_pretty(s).map_err(|e| StoreError::Io(io::Error::other(e)))?;
    crate::atomic_file::write_atomic(path, json.as_bytes()).map_err(StoreError::Io)?;
    let modified = fs::metadata(path).ok().and_then(|m| m.modified().ok());
    Ok(Stamp { modified, len: json.len() as u64, hash: hash(json.as_bytes()) })
}

/// Apply `patch` to the file at `path` as it is now and write the result.
/// Returns the new settings, the keys that were refused, and the stamp of
/// what was written. A file that doesn't parse is left alone.
pub fn update_at(path: &Path, patch: &Patch) -> Result<(AppSettings, Vec<String>, Stamp), StoreError> {
    let (on_disk, _) = read(path)?;
    let (new, refused) = apply(&on_disk, patch);
    let stamp = write(path, &new)?;
    Ok((new, refused, stamp))
}

/// Replace the whole file (Import). Still refuses nothing: an import is the
/// user asking for exactly these settings.
pub fn replace_at(path: &Path, s: &AppSettings) -> Result<Stamp, StoreError> {
    let mut s = s.clone();
    s.clamp();
    write(path, &s)
}

/// `settings.json`'s path.
pub fn path() -> Option<PathBuf> { AppSettings::settings_path() }

/// [`update_at`] on the real file.
pub fn update(patch: &Patch) -> Result<(AppSettings, Vec<String>, Stamp), StoreError> {
    let p = path().ok_or_else(|| StoreError::Io(io::Error::other("no config dir")))?;
    update_at(&p, patch)
}

/// [`replace_at`] on the real file.
pub fn replace(s: &AppSettings) -> Result<Stamp, StoreError> {
    let p = path().ok_or_else(|| StoreError::Io(io::Error::other("no config dir")))?;
    replace_at(&p, s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::ToolStyle;
    use crate::tool::Tool;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ff-store-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("settings.json")
    }

    #[test]
    fn a_patch_holds_only_what_changed() {
        let a = AppSettings::default();
        let mut b = a.clone();
        b.theme = "Orange".into();
        b.set_tool_style(Tool::Arrow, ToolStyle { color_argb: 1, thickness: 5.0 });
        let p = diff(&a, &b);
        assert_eq!(p.keys().collect::<Vec<_>>(), vec!["Theme", "ToolStyles"]);
        assert_eq!(p["ToolStyles"].as_object().unwrap().keys().collect::<Vec<_>>(), vec!["Arrow"]);
        assert!(diff(&a, &a).is_empty());
    }

    #[test]
    fn two_holders_changing_different_tools_keep_both() {
        let path = temp("tools");
        let base = AppSettings::default();
        // The Settings window imported a Pen style; the editor, opened
        // before that, then picks an Arrow color.
        let mut imported = base.clone();
        imported.set_tool_style(Tool::Pen, ToolStyle { color_argb: 7, thickness: 2.0 });
        replace_at(&path, &imported).unwrap();
        let mut editor = base.clone();
        editor.set_tool_style(Tool::Arrow, ToolStyle { color_argb: 9, thickness: 4.0 });
        let (after, refused, _) = update_at(&path, &diff(&base, &editor)).unwrap();
        assert!(refused.is_empty());
        assert_eq!(after.tool_style(Tool::Pen).color_argb, 7, "the import survives");
        assert_eq!(after.tool_style(Tool::Arrow).color_argb, 9, "the editor's change lands");
    }

    #[test]
    fn a_stale_holder_never_writes_back_what_it_did_not_change() {
        let path = temp("stale");
        let base = AppSettings::default();
        let mut hand = base.clone();
        hand.auto_copy = true;
        replace_at(&path, &hand).unwrap();
        // The tray, still holding `base`, changes the sound.
        let mut tray = base.clone();
        tray.record_sound = "Mic".into();
        let (after, _, _) = update_at(&path, &diff(&base, &tray)).unwrap();
        assert!(after.auto_copy, "the hand edit stays");
        assert_eq!(after.record_sound, "Mic");
    }

    #[test]
    fn a_file_that_does_not_parse_is_never_written_over() {
        let path = temp("broken");
        fs::write(&path, "{ \"Theme\": ").unwrap();
        let mut p = Patch::new();
        p.insert("Theme".into(), Value::String("Green".into()));
        assert!(matches!(update_at(&path, &p), Err(StoreError::Unparsable(_))));
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ \"Theme\": ");
    }

    #[test]
    fn a_bad_key_is_refused_alone() {
        let mut p = Patch::new();
        p.insert("AutoCopy".into(), Value::String("yes".into()));
        p.insert("Theme".into(), Value::String("Cyan".into()));
        let (s, refused) = apply(&AppSettings::default(), &p);
        assert_eq!(refused, vec!["AutoCopy".to_owned()]);
        assert_eq!(s.theme, "Cyan");
    }

    #[test]
    fn merged_patches_keep_the_later_value_and_every_entry() {
        let a = AppSettings::default();
        let mut b = a.clone();
        b.set_tool_style(Tool::Pen, ToolStyle { color_argb: 1, thickness: 1.0 });
        b.theme = "Green".into();
        let mut c = a.clone();
        c.set_tool_style(Tool::Arrow, ToolStyle { color_argb: 2, thickness: 2.0 });
        c.theme = "Cyan".into();
        let mut p = diff(&a, &b);
        merge(&mut p, diff(&a, &c));
        let (s, _) = apply(&a, &p);
        assert_eq!(s.theme, "Cyan");
        assert_eq!(s.tool_style(Tool::Pen).color_argb, 1);
        assert_eq!(s.tool_style(Tool::Arrow).color_argb, 2);
    }

    #[test]
    fn a_stamp_changes_when_the_content_does() {
        let path = temp("stamp");
        let s1 = replace_at(&path, &AppSettings::default()).unwrap();
        assert_eq!(stamp_of(&path), Some(s1));
        let mut other = AppSettings::default();
        other.theme = "Yellow".into();
        let s2 = replace_at(&path, &other).unwrap();
        assert_ne!(s1.hash, s2.hash);
    }
}
