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
    /// Something the user should know before installing (e.g. a snap that needs other snaps).
    pub warning: Option<String>,
}

/// Per-install switches.
#[derive(Debug, Default, Clone)]
pub struct Opts {
    /// Treat a Windows .exe as an installer to run under Wine (instead of a portable app).
    pub installer: bool,
    /// Extra arguments for a Windows installer, e.g. `/S` (NSIS), `/VERYSILENT` (Inno), `/qn` (msi).
    pub installer_args: Vec<String>,
}

pub trait Backend {
    fn kind(&self) -> &'static str;
    /// Cheap check by extension/magic bytes.
    fn detect(&self, path: &Path) -> bool;
    fn inspect(&self, path: &Path) -> Result<Info>;
    fn install(&self, path: &Path, dirs: &Dirs, opts: &Opts) -> Result<Manifest>;
}

pub mod appimage;
pub mod arch;
pub mod archive;
pub mod deb;
pub mod elfbin;
pub mod exe;
pub mod flatpak;
pub mod jar;
pub mod rpm;
pub mod snap;
pub mod tree;

pub fn all() -> Vec<Box<dyn Backend>> {
    vec![Box::new(deb::Deb), Box::new(appimage::AppImage), Box::new(flatpak::Flatpak), Box::new(exe::Exe), Box::new(rpm::Rpm), Box::new(arch::Arch), Box::new(snap::Snap), Box::new(archive::Archive), Box::new(jar::Jar), Box::new(archive::Makeself), Box::new(elfbin::ElfBin)]
}

pub fn pick(path: &Path) -> Option<Box<dyn Backend>> {
    all().into_iter().find(|b| b.detect(path))
}

/// Lowercase, filesystem-safe app id.
pub fn slug(s: &str) -> String {
    // `+` is common in names (Notepad++, C++) and means something: `notepadpp`, not `notepad--`
    s.replace('+', "p")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' { c.to_ascii_lowercase() } else { '-' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::slug;

    #[test]
    fn slugs() {
        assert_eq!(slug("Notepad++"), "notepadpp");
        assert_eq!(slug("Hello World_1.2"), "hello-world_1.2");
        assert_eq!(slug("org.test.App"), "org.test.app");
    }
}
