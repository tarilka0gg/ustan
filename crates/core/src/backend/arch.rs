//! Arch Linux packages (`*.pkg.tar.zst|xz|gz|bz2`): a tar with a `.PKGINFO` and files rooted at `/`.
use super::tree::{decompress, install_tree, Spec};
use super::{slug, Backend, Info, Opts};
use crate::{dirs::Dirs, manifest::Manifest, Error, Result};
use std::io::Read;
use std::path::Path;

pub struct Arch;

fn is_arch(path: &Path) -> bool {
    let n = path.file_name().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    [".pkg.tar.zst", ".pkg.tar.xz", ".pkg.tar.gz", ".pkg.tar.bz2", ".pkg.tar"].iter().any(|e| n.ends_with(e))
}

struct Meta {
    name: String,
    version: String,
}

fn read_tar(path: &Path) -> Result<Vec<u8>> {
    let data = std::fs::read(path)?;
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    decompress(&ext, &data)
}

fn meta(tar: &[u8]) -> Result<Meta> {
    let mut ar = tar::Archive::new(tar);
    for e in ar.entries()? {
        let mut e = e?;
        if e.path()?.as_ref() == Path::new(".PKGINFO") {
            let mut s = String::new();
            e.read_to_string(&mut s)?;
            let get = |k: &str| s.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix(" = ")).map(|v| v.trim().to_string());
            return Ok(Meta {
                name: get("pkgname").ok_or_else(|| Error::Format(".PKGINFO has no pkgname".into()))?,
                version: get("pkgver").unwrap_or_default(),
            });
        }
    }
    Err(Error::Format("no .PKGINFO: not an Arch package".into()))
}

impl Backend for Arch {
    fn kind(&self) -> &'static str {
        "arch"
    }

    fn detect(&self, path: &Path) -> bool {
        is_arch(path)
    }

    fn inspect(&self, path: &Path) -> Result<Info> {
        let m = meta(&read_tar(path)?)?;
        Ok(Info { id: slug(&m.name), name: m.name, version: Some(m.version), kind: "arch", icon: None, warning: None })
    }

    fn install(&self, path: &Path, dirs: &Dirs, opts: &Opts) -> Result<Manifest> {
        let tar = read_tar(path)?;
        let m = meta(&tar)?;
        let spec = Spec { kind: "arch", id: slug(&m.name), name: m.name, version: Some(m.version), source: path, overlay: true };
        install_tree(
            spec,
            dirs,
            opts,
            |root| {
                let mut ar = tar::Archive::new(tar.as_slice());
                ar.set_preserve_permissions(true);
                for e in ar.entries()? {
                    let mut e = e?;
                    // .PKGINFO, .MTREE, .BUILDINFO, .INSTALL...: package metadata, not files.
                    if e.path()?.components().next().is_some_and(|c| c.as_os_str().to_string_lossy().starts_with('.')) {
                        continue;
                    }
                    e.unpack_in(root)?;
                }
                Ok(())
            },
            |_, _, _, _| Ok(()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_arch_names() {
        assert!(is_arch(Path::new("/x/hello-2.12-1-x86_64.pkg.tar.zst")));
        assert!(is_arch(Path::new("A.PKG.TAR.XZ")));
        assert!(!is_arch(Path::new("tool.tar.gz")));
    }
}
