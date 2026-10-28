//! Per-user state of the desktop app that is not configuration (like RustDesk_local.toml):
//! recent connections, favorite IDs and window geometry. Stored as `local.toml` next to the
//! agent config with owner-only permissions; a missing or broken file just means "empty".

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub const MAX_RECENT: usize = 30;
const MAX_FAVORITES: usize = 200;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recent {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Unix time (seconds) of the last connection.
    #[serde(default)]
    pub last: u64,
    #[serde(default)]
    pub count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowGeom {
    /// Outer position in points; None where the platform cannot report it (Wayland).
    #[serde(default)]
    pub pos: Option<[f32; 2]>,
    /// Inner size in points.
    pub size: [f32; 2],
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LocalState {
    /// Most recent first.
    #[serde(default)]
    pub recent: Vec<Recent>,
    #[serde(default)]
    pub favorites: Vec<String>,
    #[serde(default)]
    pub main_window: Option<WindowGeom>,
    #[serde(default)]
    pub viewer_size: Option<[f32; 2]>,
    /// Device list as rows instead of cards.
    #[serde(default)]
    pub peer_list: bool,
}

/// A device ID as typed or linked: exactly nine digits once spaces are removed.
pub fn normalize_id(s: &str) -> Option<String> {
    let id: String = s.chars().filter(|c| *c != ' ').collect();
    (id.len() == 9 && id.bytes().all(|b| b.is_ascii_digit())).then_some(id)
}

/// `redesk://connect/<id>` or `redesk://<id>` (one trailing slash allowed, URL-encoded spaces
/// accepted). Anything else is rejected.
pub fn parse_link(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("redesk") {
        return None;
    }
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let rest = match rest.get(..8) {
        Some(p) if p.eq_ignore_ascii_case("connect/") => &rest[8..],
        _ => rest,
    };
    normalize_id(&rest.replace("%20", " "))
}

fn sane_size(s: [f32; 2], min: [f32; 2]) -> bool {
    s.iter().zip(min).all(|(v, m)| v.is_finite() && *v >= m && *v <= 16384.0)
}

fn sane_pos(p: [f32; 2]) -> bool {
    p.iter().all(|v| v.is_finite() && v.abs() <= 32768.0)
}

impl LocalState {
    pub fn path_for(config_path: &Path) -> PathBuf {
        config_path.with_file_name("local.toml")
    }

    pub fn load(path: &Path) -> Self {
        let Ok(s) = std::fs::read_to_string(path) else { return Self::default() };
        match toml::from_str::<Self>(&s) {
            Ok(mut st) => {
                st.sanitize();
                st
            }
            Err(e) => {
                tracing::warn!("ignoring invalid {}: {e}", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Drops anything a hand-edited or corrupted file could carry that the UI should not trust.
    fn sanitize(&mut self) {
        self.recent.retain(|r| normalize_id(&r.id).as_deref() == Some(r.id.as_str()));
        for r in &mut self.recent {
            r.name = r.name.chars().take(64).collect();
        }
        self.recent.sort_by_key(|r| std::cmp::Reverse(r.last));
        let mut seen = std::collections::HashSet::new();
        self.recent.retain(|r| seen.insert(r.id.clone()));
        self.recent.truncate(MAX_RECENT);
        let mut seen = std::collections::HashSet::new();
        self.favorites.retain(|f| normalize_id(f).as_deref() == Some(f.as_str()) && seen.insert(f.clone()));
        self.favorites.truncate(MAX_FAVORITES);
        if let Some(g) = &mut self.main_window {
            if !sane_size(g.size, [200.0, 150.0]) {
                self.main_window = None;
            } else if g.pos.is_some_and(|p| !sane_pos(p)) {
                g.pos = None;
            }
        }
        if self.viewer_size.is_some_and(|s| !sane_size(s, [200.0, 150.0])) {
            self.viewer_size = None;
        }
    }

    /// Records a connection to `id` at unix time `now`, keeping the newest MAX_RECENT.
    pub fn record(&mut self, id: &str, name: &str, now: u64) {
        let mut entry = match self.recent.iter().position(|r| r.id == id) {
            Some(i) => self.recent.remove(i),
            None => Recent { id: id.to_string(), name: String::new(), last: 0, count: 0 },
        };
        if !name.trim().is_empty() {
            entry.name = name.chars().take(64).collect();
        }
        entry.last = now;
        entry.count = entry.count.saturating_add(1);
        self.recent.insert(0, entry);
        self.recent.truncate(MAX_RECENT);
    }

    pub fn remove_recent(&mut self, id: &str) {
        self.recent.retain(|r| r.id != id);
    }

    pub fn is_favorite(&self, id: &str) -> bool {
        self.favorites.iter().any(|f| f == id)
    }

    /// Returns the new state.
    pub fn toggle_favorite(&mut self, id: &str) -> bool {
        if self.is_favorite(id) {
            self.favorites.retain(|f| f != id);
            false
        } else {
            if self.favorites.len() >= MAX_FAVORITES {
                self.favorites.remove(0);
            }
            self.favorites.push(id.to_string());
            true
        }
    }

    /// Last known display name for `id`, if any.
    pub fn name_of(&self, id: &str) -> Option<&str> {
        self.recent.iter().find(|r| r.id == id).map(|r| r.name.as_str()).filter(|n| !n.is_empty())
    }
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links() {
        assert_eq!(parse_link("redesk://connect/123456789").as_deref(), Some("123456789"));
        assert_eq!(parse_link("redesk://123456789").as_deref(), Some("123456789"));
        assert_eq!(parse_link("redesk://123456789/").as_deref(), Some("123456789"));
        assert_eq!(parse_link("ReDesk://Connect/123 456 789").as_deref(), Some("123456789"));
        assert_eq!(parse_link("redesk://connect/123%20456%20789").as_deref(), Some("123456789"));
        assert_eq!(parse_link(" redesk://123456789 ").as_deref(), Some("123456789"));
        for bad in [
            "redesk://12345678",
            "redesk://1234567890",
            "redesk://12345678a",
            "redesk://connect/123456789?pw=x",
            "redesk://connect/123456789/extra",
            "redesk://open/123456789",
            "redesk:123456789",
            "https://123456789",
            "rustdesk://123456789",
            "redesk://",
            "redesk://connect/",
            "redesk://１２３４５６７８９",
            "redesk://123\t456789",
            "",
        ] {
            assert_eq!(parse_link(bad), None, "{bad}");
        }
    }

    #[test]
    fn recent_order_and_limit() {
        let mut s = LocalState::default();
        for i in 0..40u64 {
            s.record(&format!("{:09}", 100000000 + i), "", i);
        }
        assert_eq!(s.recent.len(), MAX_RECENT);
        assert_eq!(s.recent[0].id, "100000039");
        s.record("100000020", "บัญชี-PC01", 100);
        assert_eq!(s.recent[0].id, "100000020");
        assert_eq!(s.recent[0].count, 2);
        assert_eq!(s.recent[0].name, "บัญชี-PC01");
        assert_eq!(s.recent.len(), MAX_RECENT);
        // An empty name keeps the last known one.
        s.record("100000020", "", 101);
        assert_eq!(s.name_of("100000020"), Some("บัญชี-PC01"));
        s.remove_recent("100000020");
        assert!(s.recent.iter().all(|r| r.id != "100000020"));
    }

    #[test]
    fn favorites() {
        let mut s = LocalState::default();
        assert!(s.toggle_favorite("512340987"));
        assert!(s.is_favorite("512340987"));
        assert!(!s.toggle_favorite("512340987"));
        assert!(!s.is_favorite("512340987"));
    }

    #[test]
    fn roundtrip_and_permissions() {
        let dir = std::env::temp_dir().join(format!("redesk-local-test-{}", std::process::id()));
        let path = LocalState::path_for(&dir.join("agent.toml"));
        assert_eq!(path.file_name().unwrap(), "local.toml");
        let mut s = LocalState::default();
        s.record("512340987", "บัญชี-PC01", 1_700_000_000);
        s.toggle_favorite("734120556");
        s.main_window = Some(WindowGeom { pos: Some([120.0, 80.0]), size: [960.0, 600.0] });
        s.viewer_size = Some([1440.0, 900.0]);
        s.save(&path).unwrap();
        assert_eq!(LocalState::load(&path), s);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::write(&path, "not = [valid").unwrap();
        assert_eq!(LocalState::load(&path), LocalState::default());
        assert_eq!(LocalState::load(&dir.join("missing.toml")), LocalState::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanitize_drops_bad_data() {
        let src = r#"
            favorites = ["123456789", "123456789", "evil", "12345678x"]
            viewer_size = [1.0, 99999.0]
            [main_window]
            pos = [1e9, 5.0]
            size = [900.0, 600.0]
            [[recent]]
            id = "../../etc"
            last = 5
            [[recent]]
            id = "111222333"
            last = 1
            [[recent]]
            id = "444555666"
            last = 9
        "#;
        let mut s: LocalState = toml::from_str(src).unwrap();
        s.sanitize();
        assert_eq!(s.favorites, vec!["123456789".to_string()]);
        assert_eq!(s.viewer_size, None);
        assert_eq!(s.main_window, Some(WindowGeom { pos: None, size: [900.0, 600.0] }));
        let ids: Vec<_> = s.recent.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["444555666", "111222333"]);
    }
}
