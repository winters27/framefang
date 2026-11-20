//! What the app remembers on its own, apart from the settings the user
//! picks: kept in `state.json` beside `settings.json`, written only by the
//! tray, and never part of Import, Export or the reset marks.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppState {
    /// The rectangle the last capture took, in virtual-desktop device pixels
    /// `[x, y, w, h]`, for "Capture last region".
    #[serde(rename = "LastRegion", default, skip_serializing_if = "Option::is_none")]
    pub last_region: Option<[i32; 4]>,
}

impl AppState {
    pub fn path() -> Option<PathBuf> {
        crate::settings::AppSettings::config_dir().map(|d| d.join("state.json"))
    }

    /// The state in `path`. Anything missing, unreadable or out of range
    /// reads as nothing remembered.
    pub fn load_from(path: &Path) -> AppState {
        let mut s: AppState = fs::read_to_string(path).ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        s.last_region = s.last_region.filter(|r| valid_rect(*r));
        s
    }

    pub fn load() -> AppState {
        Self::path().map(|p| Self::load_from(&p)).unwrap_or_default()
    }

    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() { fs::create_dir_all(dir)?; }
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        crate::atomic_file::write_atomic(path, json.as_bytes())
    }

    pub fn save(&self) -> io::Result<()> {
        let p = Self::path().ok_or_else(|| io::Error::other("no config dir"))?;
        self.save_to(&p)
    }
}

/// A rectangle a capture could have taken: positive size, and no edge past
/// what `i32` math can reach.
pub fn valid_rect(r: [i32; 4]) -> bool {
    let [x, y, w, h] = r;
    w > 0 && h > 0 && w <= 1 << 16 && h <= 1 << 16 && x.checked_add(w).is_some() && y.checked_add(h).is_some()
        && x.abs() < 1 << 20 && y.abs() < 1 << 20
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_drops_nonsense() {
        let dir = std::env::temp_dir().join(format!("ff-state-{}", std::process::id()));
        let path = dir.join("state.json");
        let s = AppState { last_region: Some([-1920, 0, 800, 600]) };
        s.save_to(&path).unwrap();
        assert_eq!(AppState::load_from(&path), s);
        fs::write(&path, "{\"LastRegion\":[0,0,2147483647,1]}").unwrap();
        assert_eq!(AppState::load_from(&path).last_region, None, "a size i32 math overflows on is dropped");
        fs::write(&path, "{\"LastRegion\":[0,0,0,10]}").unwrap();
        assert_eq!(AppState::load_from(&path).last_region, None);
        fs::write(&path, "not json").unwrap();
        assert_eq!(AppState::load_from(&path), AppState::default());
        assert_eq!(AppState::load_from(&dir.join("missing.json")), AppState::default());
        let _ = fs::remove_dir_all(&dir);
    }
}
