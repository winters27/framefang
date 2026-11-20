//! How saved screenshots and recordings are named.
//!
//! A pattern is text with tokens in braces, set in Settings (three presets)
//! or by hand in `settings.json` (`FileNamePattern`):
//!
//! | Token | Gives |
//! | --- | --- |
//! | `{yyyy}` `{MM}` `{dd}` `{HH}` `{mm}` `{ss}` | the parts of the date and time |
//! | `{date}` | `2026-11-19` |
//! | `{time}` | `11-42-07` |
//! | `{app}` | the app whose window was captured, else nothing |
//! | `{title}` | that window's title, else nothing |
//! | `{w}` `{h}` | the picture's size in pixels |
//! | `{n}` | the first number that gives a free name in the folder |
//!
//! Unknown tokens stay as written. The result is cleaned for every file
//! system Frame Fang runs on: characters Windows refuses are dropped, the
//! separators an empty token leaves behind are collapsed, and an empty
//! result falls back to the default pattern.
//!
//! [`claim`] then creates the file with `create_new`, so a name is never
//! written over: two captures in one second become `name` and `name (2)`.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Local, Timelike};

/// The name every version before patterns used: `framefang_20261119_114207`.
pub const DEFAULT_PATTERN: &str = "framefang_{yyyy}{MM}{dd}_{HH}{mm}{ss}";

/// Settings' three presets, in chip order: (chip word, pattern).
pub const PRESETS: [(&str, &str); 3] = [
    ("FRAME FANG", DEFAULT_PATTERN),
    ("DATE", "{date} {time}"),
    ("APP + DATE", "{app} {date} {time}"),
];

/// Longest name (without extension) a pattern may give.
pub const MAX_LEN: usize = 120;

/// What a name can be built from. Missing facts are empty strings or 0.
#[derive(Debug, Clone)]
pub struct NameFacts {
    pub now: DateTime<Local>,
    pub app: String,
    pub title: String,
    pub w: u32,
    pub h: u32,
}

impl NameFacts {
    /// Only the time: full-screen captures, recordings, anything without a
    /// window behind it.
    pub fn at(now: DateTime<Local>, w: u32, h: u32) -> NameFacts {
        NameFacts { now, app: String::new(), title: String::new(), w, h }
    }
}

/// The preset `pattern` is, if any (by index into [`PRESETS`]).
pub fn preset_index(pattern: &str) -> Option<usize> {
    PRESETS.iter().position(|(_, p)| *p == pattern)
}

/// `pattern` with its tokens filled in and cleaned, for name number `n`
/// (`{n}` gives `n`; a pattern without `{n}` ignores it).
pub fn render_numbered(pattern: &str, f: &NameFacts, n: u32) -> String {
    let d = f.now;
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let token = &after[..close];
        let value = match token {
            "yyyy" => Some(format!("{:04}", d.year())),
            "MM" => Some(format!("{:02}", d.month())),
            "dd" => Some(format!("{:02}", d.day())),
            "HH" => Some(format!("{:02}", d.hour())),
            "mm" => Some(format!("{:02}", d.minute())),
            "ss" => Some(format!("{:02}", d.second())),
            "date" => Some(format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())),
            "time" => Some(format!("{:02}-{:02}-{:02}", d.hour(), d.minute(), d.second())),
            "app" => Some(f.app.clone()),
            "title" => Some(f.title.clone()),
            "w" => Some(f.w.to_string()),
            "h" => Some(f.h.to_string()),
            "n" => Some(n.to_string()),
            _ => None,
        };
        match value {
            Some(v) => out.push_str(&v),
            None => { out.push('{'); out.push_str(token); out.push('}'); }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    let clean = sanitize(&out);
    if clean.is_empty() && pattern != DEFAULT_PATTERN {
        return render_numbered(DEFAULT_PATTERN, f, n);
    }
    clean
}

/// `pattern` rendered for the first name number.
pub fn render(pattern: &str, f: &NameFacts) -> String {
    render_numbered(pattern, f, 1)
}

/// Whether `pattern` numbers its names itself.
pub fn has_counter(pattern: &str) -> bool {
    pattern.contains("{n}")
}

/// A name every file system accepts.
pub fn sanitize(s: &str) -> String {
    const SEPARATORS: [char; 3] = [' ', '_', '-'];
    // A run of one separator (two spaces left by an empty `{app}`) becomes
    // one; separators at either end go.
    let mut out = String::with_capacity(s.len());
    let mut prev: Option<char> = None;
    for c in s.chars() {
        if c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            continue;
        }
        if SEPARATORS.contains(&c) && (prev == Some(c) || prev.is_none()) {
            continue;
        }
        out.push(c);
        prev = Some(c);
    }
    let mut out: String = out.trim_end_matches(|c: char| SEPARATORS.contains(&c) || c == '.').to_owned();
    if out.chars().count() > MAX_LEN {
        out = out.chars().take(MAX_LEN).collect();
        out = out.trim_end_matches(|c: char| SEPARATORS.contains(&c) || c == '.').to_owned();
    }
    let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
        "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) { out.push('_'); }
    out
}

/// Create a new file in `dir` named by `pattern` with extension `ext`
/// (without the dot), never touching a file that is already there. With
/// `{n}` the number counts up from 1; without it the name gets ` (2)`,
/// ` (3)` ... when taken. Returns the path and the open, empty file.
pub fn claim(dir: &Path, pattern: &str, facts: &NameFacts, ext: &str) -> io::Result<(PathBuf, File)> {
    let numbered = has_counter(pattern);
    let base = render(pattern, facts);
    for i in 1..=9999u32 {
        let counted = if numbered { Some(render_numbered(pattern, facts, i)) } else { None };
        // A name cut to the length limit can lose its `{n}`; then the
        // number goes on the end like an unnumbered name's.
        let stem = match counted {
            Some(s) if i == 1 || s != render_numbered(pattern, facts, i - 1) => s,
            _ if i == 1 => base.clone(),
            _ => format!("{base} ({i})"),
        };
        let path = dir.join(format!("{stem}.{ext}"));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => return Ok((path, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("no free file name"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn facts() -> NameFacts {
        NameFacts {
            now: Local.with_ymd_and_hms(2026, 11, 19, 11, 42, 7).unwrap(),
            app: "Chrome".into(),
            title: "Docs: notes/1".into(),
            w: 1280,
            h: 720,
        }
    }

    #[test]
    fn the_default_renders_the_old_name() {
        assert_eq!(render(DEFAULT_PATTERN, &facts()), "framefang_20261119_114207");
        assert_eq!(render("{date} {time}", &facts()), "2026-11-19 11-42-07");
        assert_eq!(render("{app} {date} {time}", &facts()), "Chrome 2026-11-19 11-42-07");
        assert_eq!(render("{w}x{h}", &facts()), "1280x720");
    }

    #[test]
    fn empty_tokens_leave_no_stray_separators() {
        let mut f = facts();
        f.app.clear();
        f.title.clear();
        assert_eq!(render("{app} {date} {time}", &f), "2026-11-19 11-42-07");
        assert_eq!(render("{date} - {app}", &f), "2026-11-19");
        assert_eq!(render("{date}_{app}_{title}", &f), "2026-11-19");
        assert_eq!(render("{date} {app} {time}", &f), "2026-11-19 11-42-07");
        assert_eq!(render("{date} - {app}", &facts()), "2026-11-19 - Chrome", "a chosen dash stays");
        assert_eq!(render("{app}", &f), "framefang_20261119_114207", "empty falls back to the default");
    }

    #[test]
    fn illegal_characters_and_reserved_names_are_cleaned() {
        assert_eq!(render("{title}", &facts()), "Docs notes1");
        assert_eq!(sanitize("a:b*c?d\"e<f>g|h\\i/j"), "abcdefghij");
        assert_eq!(sanitize("  name.  "), "name");
        assert_eq!(sanitize("CON"), "CON_");
        assert_eq!(sanitize("nul.txt"), "nul.txt_");
        assert_eq!(sanitize(&"x".repeat(200)).len(), MAX_LEN);
        assert_eq!(render("{nope} {date}", &facts()), "{nope} 2026-11-19", "unknown tokens stay");
        assert_eq!(render("half {date", &facts()), "half {date");
    }

    #[test]
    fn claim_counts_up_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("ff-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("framefang_20261119_114207.png"), b"keep").unwrap();
        let (p2, _) = claim(&dir, DEFAULT_PATTERN, &facts(), "png").unwrap();
        assert_eq!(p2.file_name().unwrap(), "framefang_20261119_114207 (2).png");
        assert_eq!(std::fs::read(dir.join("framefang_20261119_114207.png")).unwrap(), b"keep");
        let (p3, _) = claim(&dir, DEFAULT_PATTERN, &facts(), "png").unwrap();
        assert_eq!(p3.file_name().unwrap(), "framefang_20261119_114207 (3).png");
        let (n1, _) = claim(&dir, "shot {n}", &facts(), "png").unwrap();
        let (n2, _) = claim(&dir, "shot {n}", &facts(), "png").unwrap();
        assert_eq!((n1.file_name().unwrap(), n2.file_name().unwrap()), ("shot 1.png".as_ref(), "shot 2.png".as_ref()));
        // A title so long the number is cut off still gives a new name.
        let mut long = facts();
        long.title = "t".repeat(200);
        let (a, _) = claim(&dir, "{title} {n}", &long, "png").unwrap();
        let (b, _) = claim(&dir, "{title} {n}", &long, "png").unwrap();
        assert_ne!(a, b);
        assert!(b.to_string_lossy().ends_with(" (2).png"), "{}", b.display());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn presets_are_found_by_pattern() {
        assert_eq!(preset_index(DEFAULT_PATTERN), Some(0));
        assert_eq!(preset_index("{date} {time}"), Some(1));
        assert_eq!(preset_index("mine {n}"), None);
    }
}
