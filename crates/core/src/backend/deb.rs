use super::tree::{decompress, install_tree, Spec};
use super::{slug, Backend, Info, Opts};
use crate::{ar, dirs::Dirs, manifest::Manifest, Error, Result};
use std::io::Read;
use std::path::Path;

pub struct Deb;

/// Name of the first archive member matching `prefix*`, via the Zig parser plus a header scan.
fn member<'a>(data: &'a [u8], prefix: &str) -> Result<(String, &'a [u8])> {
    let r = ar::find(data, &format!("{prefix}*"))?;
    // ar names are 16 bytes at (offset - 60).
    let name = std::str::from_utf8(&data[r.start - 60..r.start - 44])
        .map_err(|_| Error::Format("bad member name".into()))?
        .trim_end()
        .trim_end_matches('/')
        .to_string();
    Ok((name, &data[r]))
}

#[derive(Default)]
struct Control {
    package: String,
    version: Option<String>,
}

fn read_control(data: &[u8]) -> Result<Control> {
    let (name, body) = member(data, "control.tar")?;
    let tar_bytes = decompress(&name, body)?;
    let mut ar = tar::Archive::new(tar_bytes.as_slice());
    for e in ar.entries()? {
        let mut e = e?;
        if e.path()?.file_name().is_some_and(|n| n == "control") {
            let mut s = String::new();
            e.read_to_string(&mut s)?;
            let mut c = Control::default();
            for l in s.lines() {
                if let Some(v) = l.strip_prefix("Package:") {
                    c.package = v.trim().to_string();
                } else if let Some(v) = l.strip_prefix("Version:") {
                    c.version = Some(v.trim().to_string());
                }
            }
            if c.package.is_empty() {
                return Err(Error::Format("control has no Package".into()));
            }
            return Ok(c);
        }
    }
    Err(Error::Format("no control file".into()))
}

impl Backend for Deb {
    fn kind(&self) -> &'static str {
        "deb"
    }

    fn detect(&self, path: &Path) -> bool {
        path.extension().is_some_and(|e| e == "deb")
    }

    fn inspect(&self, path: &Path) -> Result<Info> {
        let data = std::fs::read(path)?;
        let c = read_control(&data)?;
        Ok(Info { id: slug(&c.package), name: c.package, version: c.version, kind: "deb", icon: None, warning: None })
    }

    fn install(&self, path: &Path, dirs: &Dirs, opts: &Opts) -> Result<Manifest> {
        let data = std::fs::read(path)?;
        let c = read_control(&data)?;
        let (name, body) = member(&data, "data.tar")?;
        let tar_bytes = decompress(&name, body)?;
        let spec = Spec { kind: "deb", id: slug(&c.package), name: c.package, version: c.version, source: path, overlay: true };
        install_tree(
            spec,
            dirs,
            opts,
            |root| {
                // unpack_in refuses entries that escape `root`.
                let mut ar = tar::Archive::new(tar_bytes.as_slice());
                ar.set_preserve_permissions(true);
                for e in ar.entries()? {
                    e?.unpack_in(root)?;
                }
                Ok(())
            },
            |_, _, _, _| Ok(()),
        )
    }
}
