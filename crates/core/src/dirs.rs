use std::path::{Path, PathBuf};

/// Where ustan puts things. Everything is per-user, nothing needs root.
#[derive(Debug, Clone)]
pub struct Dirs {
    pub opt: PathBuf,
    pub apps: PathBuf,
    pub state: PathBuf,
}

impl Dirs {
    pub fn under(home: &Path) -> Self {
        Dirs {
            opt: home.join(".local/opt"),
            apps: home.join(".local/share/applications"),
            state: home.join(".local/share/ustan"),
        }
    }

    pub fn from_env() -> Self {
        Self::under(&PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into())))
    }
}
