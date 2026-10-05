use super::{slug, Backend, Info, Opts};
use crate::{ar, desktop, dirs::Dirs, manifest::Manifest, Error, Result};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

pub struct Deb;

fn decompress(name: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    if name.ends_with(".gz") {
        flate2::read::GzDecoder::new(bytes).read_to_end(&mut out)?;
    } else if name.ends_with(".xz") {
        lzma_rs::xz_decompress(&mut Cursor::new(bytes), &mut out).map_err(|e| Error::Format(format!("xz: {e}")))?;
    } else if name.ends_with(".zst") {
        ruzstd::decoding::StreamingDecoder::new(bytes)
            .map_err(|e| Error::Format(format!("zstd: {e}")))?
            .read_to_end(&mut out)?;
    } else if name.ends_with(".tar") {
        out.extend_from_slice(bytes);
    } else {
        return Err(Error::Format(format!("unsupported compression: {name}")));
    }
    Ok(out)
}

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
        Ok(Info { id: slug(&c.package), name: c.package, version: c.version, kind: "deb", icon: None })
    }

    fn install(&self, path: &Path, dirs: &Dirs, _opts: &Opts) -> Result<Manifest> {
        let data = std::fs::read(path)?;
        let c = read_control(&data)?;
        let id = slug(&c.package);
        let root = dirs.opt.join(&id);
        if root.exists() {
            return Err(Error::Format(format!("`{id}` is already installed")));
        }

        let (name, body) = member(&data, "data.tar")?;
        let tar_bytes = decompress(&name, body)?;
        std::fs::create_dir_all(&root)?;
        // unpack_in refuses entries that escape `root`.
        let mut files: Vec<PathBuf> = vec![root.clone()];
        let res = (|| -> Result<()> {
            let mut ar = tar::Archive::new(tar_bytes.as_slice());
            ar.set_preserve_permissions(true);
            for e in ar.entries()? {
                e?.unpack_in(&root)?;
            }
            std::fs::create_dir_all(&dirs.apps)?;
            let apps = root.join("usr/share/applications");
            if apps.is_dir() {
                for e in std::fs::read_dir(&apps)? {
                    let p = e?.path();
                    if p.extension().is_some_and(|x| x == "desktop") {
                        let text = std::fs::read_to_string(&p)?;
                        let dst = dirs.apps.join(format!("ustan-{id}-{}", p.file_name().unwrap().to_string_lossy()));
                        std::fs::write(&dst, desktop::rewrite(&text, &root))?;
                        files.push(dst);
                    }
                }
            }
            Ok(())
        })();
        if let Err(e) = res {
            for f in files.iter().rev() {
                let _ = if f.is_dir() { std::fs::remove_dir_all(f) } else { std::fs::remove_file(f) };
            }
            return Err(e);
        }
        let m = Manifest { id, name: c.package, version: c.version, kind: "deb".into(), source: Some(path.display().to_string()), files, uninstall_cmd: vec![] };
        m.save(&dirs.state)?;
        Ok(m)
    }
}
