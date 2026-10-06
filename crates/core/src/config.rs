//! `~/.local/share/ustan/config.toml`: the few user choices ustan remembers.
use crate::{dirs::Dirs, Error, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoUpdate {
    /// How often `ustan watch` looks for updates.
    pub interval_hours: u64,
    /// Install updates by itself; otherwise only tell the user about them.
    pub apply: bool,
}

impl Default for AutoUpdate {
    fn default() -> Self {
        AutoUpdate { interval_hours: 12, apply: false }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Runner name (as listed by `ustan runner`) or a path to a `wine`/`wine64` binary.
    pub wine: Option<String>,
    pub autoupdate: AutoUpdate,
    /// ustan has made itself the default handler once (or the user opted out): never do it unasked again.
    pub registered: bool,
}

pub fn path(dirs: &Dirs) -> PathBuf {
    dirs.state.join("config.toml")
}

/// Missing or unreadable config means defaults: a broken file must never stop an install.
pub fn load(dirs: &Dirs) -> Config {
    std::fs::read_to_string(path(dirs)).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
}

pub fn save(dirs: &Dirs, c: &Config) -> Result<()> {
    std::fs::create_dir_all(&dirs.state)?;
    let text = toml::to_string_pretty(c).map_err(|e| Error::Manifest(e.to_string()))?;
    std::fs::write(path(dirs), text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_survive_partial_and_broken_files() {
        let h = std::env::temp_dir().join(format!("ustan-cfg-{}", std::process::id()));
        let d = Dirs::under(&h);
        assert_eq!(load(&d).autoupdate.interval_hours, 12); // no file
        std::fs::create_dir_all(&d.state).unwrap();
        std::fs::write(path(&d), "wine = \"x\"\n").unwrap(); // only one key
        let c = load(&d);
        assert_eq!((c.wine.as_deref(), c.autoupdate.interval_hours, c.autoupdate.apply), (Some("x"), 12, false));
        std::fs::write(path(&d), "this is not toml [[[").unwrap();
        assert!(load(&d).wine.is_none());
        let mut c = Config::default();
        c.autoupdate.apply = true;
        save(&d, &c).unwrap();
        assert!(load(&d).autoupdate.apply);
        let _ = std::fs::remove_dir_all(h);
    }
}
