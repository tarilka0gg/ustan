use crate::{dirs::Dirs, manifest::Manifest, Result};
use std::path::Path;

/// An icon found inside a package.
#[derive(Clone)]
pub struct Icon {
    pub ext: &'static str,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for Icon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Icon({}, {} bytes)", self.ext, self.bytes.len())
    }
}

/// What a backend learned about a package before installing it.
#[derive(Debug)]
pub struct Info {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub kind: &'static str,
    pub icon: Option<Icon>,
}

/// Per-install switches.
#[derive(Debug, Default, Clone)]
pub struct Opts {
    /// Treat a Windows .exe as an installer to run under Wine (instead of a portable app).
    pub installer: bool,
}

pub trait Backend {
    fn kind(&self) -> &'static str;
    /// Cheap check by extension/magic bytes.
    fn detect(&self, path: &Path) -> bool;
    fn inspect(&self, path: &Path) -> Result<Info>;
    fn install(&self, path: &Path, dirs: &Dirs, opts: &Opts) -> Result<Manifest>;
}

pub mod appimage;
pub mod deb;
pub mod exe;
pub mod flatpak;

pub fn all() -> Vec<Box<dyn Backend>> {
    vec![Box::new(deb::Deb), Box::new(appimage::AppImage), Box::new(flatpak::Flatpak), Box::new(exe::Exe)]
}

pub fn pick(path: &Path) -> Option<Box<dyn Backend>> {
    all().into_iter().find(|b| b.detect(path))
}

/// Lowercase, filesystem-safe app id.
pub fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' { c.to_ascii_lowercase() } else { '-' })
        .collect()
}
