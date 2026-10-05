//! Shared logic for packages that are unpacked into a private root (`~/.local/opt/<id>`):
//! deb, rpm, Arch packages and plain archives.
use super::Opts;
use crate::{desktop, dirs::Dirs, manifest::Manifest, Error, Result};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

pub struct Spec<'a> {
    pub kind: &'static str,
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub source: &'a Path,
}

/// First `cmd` found in PATH.
pub fn which(cmd: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?.to_str()?.split(':').map(|d| Path::new(d).join(cmd)).find(|p| p.is_file())
}

/// Decompress by file/member name suffix: `.gz`/`.tgz`, `.xz`, `.zst`, `.bz2`, or plain.
pub fn decompress(name: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    let n = name.to_lowercase();
    let mut out = Vec::new();
    if n.ends_with(".gz") || n.ends_with(".tgz") {
        flate2::read::GzDecoder::new(bytes).read_to_end(&mut out)?;
    } else if n.ends_with(".xz") {
        lzma_rs::xz_decompress(&mut Cursor::new(bytes), &mut out).map_err(|e| Error::Format(format!("xz: {e}")))?;
    } else if n.ends_with(".zst") {
        ruzstd::decoding::StreamingDecoder::new(bytes).map_err(|e| Error::Format(format!("zstd: {e}")))?.read_to_end(&mut out)?;
    } else if n.ends_with(".bz2") {
        bzip2::read::BzDecoder::new(bytes).read_to_end(&mut out)?;
    } else if n.ends_with(".tar") || n.ends_with(".cpio") {
        out.extend_from_slice(bytes);
    } else {
        return Err(Error::Format(format!("unsupported compression: {name}")));
    }
    Ok(out)
}

/// Relative path that leads from directory `from` to `to` (both absolute, same root).
pub fn relative(from: &Path, to: &Path) -> PathBuf {
    let f: Vec<_> = from.components().collect();
    let t: Vec<_> = to.components().collect();
    let common = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..f.len() {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c.as_os_str());
    }
    out
}

/// Packages assume they live in `/`: an absolute symlink like `/usr/bin/x -> /opt/x/x` must
/// point into our private root instead, or it dangles (or worse, silently hits the host's copy).
pub fn fix_symlinks(root: &Path) {
    fn walk(dir: &Path, root: &Path) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                walk(&p, root);
            } else if ft.is_symlink() {
                let Ok(t) = std::fs::read_link(&p) else { continue };
                if !t.is_absolute() {
                    continue;
                }
                let inside = root.join(t.strip_prefix("/").unwrap_or(&t));
                if std::fs::symlink_metadata(&inside).is_ok() {
                    let rel = relative(p.parent().unwrap(), &inside);
                    let _ = std::fs::remove_file(&p);
                    let _ = std::os::unix::fs::symlink(rel, &p);
                }
            }
        }
    }
    walk(root, root);
}

/// Safe relative path for an archive entry: no absolute paths, no `..`, `./` stripped.
pub fn safe_rel(name: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in Path::new(name).components() {
        match c {
            std::path::Component::Normal(s) => out.push(s),
            std::path::Component::CurDir | std::path::Component::RootDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Resolve `rel` under `root`, refusing anything that would land outside it through symlinks.
pub fn dest(root: &Path, canon_root: &Path, rel: &Path) -> Result<PathBuf> {
    let full = root.join(rel);
    let parent = full.parent().unwrap_or(root);
    std::fs::create_dir_all(parent)?;
    if !parent.canonicalize()?.starts_with(canon_root) {
        return Err(Error::Format(format!("unsafe path in package: {}", rel.display())));
    }
    Ok(full)
}

pub fn remove_existing(p: &Path) {
    if let Ok(m) = std::fs::symlink_metadata(p) {
        let _ = if m.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
    }
}

fn cleanup(files: &[PathBuf]) {
    for f in files.iter().rev() {
        let _ = if f.is_dir() { std::fs::remove_dir_all(f) } else { std::fs::remove_file(f) };
    }
}

/// Unpack into `~/.local/opt/<id>` via `extract`, fix symlinks, turn the package's own
/// `usr/share/applications/*.desktop` into launchers, then let `after` add anything else
/// (it sees the list of launchers made so far). Everything is rolled back on error.
pub fn install_tree(
    spec: Spec<'_>,
    dirs: &Dirs,
    _opts: &Opts,
    extract: impl FnOnce(&Path) -> Result<()>,
    after: impl FnOnce(&Path, &Dirs, &str, &mut Vec<PathBuf>) -> Result<()>,
) -> Result<Manifest> {
    let root = dirs.opt.join(&spec.id);
    if root.exists() {
        return Err(Error::Format(format!("`{}` is already installed", spec.id)));
    }
    std::fs::create_dir_all(&root)?;
    let mut files = vec![root.clone()];
    let res = (|| -> Result<()> {
        extract(&root)?;
        fix_symlinks(&root);
        std::fs::create_dir_all(&dirs.apps)?;
        let apps = root.join("usr/share/applications");
        if apps.is_dir() {
            for e in std::fs::read_dir(&apps)? {
                let p = e?.path();
                if p.extension().is_some_and(|x| x == "desktop") {
                    let text = std::fs::read_to_string(&p)?;
                    let dst = dirs.apps.join(format!("ustan-{}-{}", spec.id, p.file_name().unwrap().to_string_lossy()));
                    std::fs::write(&dst, desktop::rewrite(&text, &root))?;
                    files.push(dst);
                }
            }
        }
        after(&root, dirs, &spec.id, &mut files)
    })();
    if let Err(e) = res {
        cleanup(&files);
        return Err(e);
    }
    let m = Manifest {
        id: spec.id,
        name: spec.name,
        version: spec.version,
        kind: spec.kind.into(),
        source: Some(spec.source.display().to_string()),
        files,
        ..Default::default()
    };
    m.save(&dirs.state)?;
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert_eq!(relative(Path::new("/r/usr/bin"), Path::new("/r/opt/x/x")), PathBuf::from("../../opt/x/x"));
        assert_eq!(relative(Path::new("/r/a"), Path::new("/r/a/b")), PathBuf::from("b"));
    }

    #[test]
    fn absolute_symlinks_stay_inside_root() {
        let r = std::env::temp_dir().join(format!("ustan-s-{}", std::process::id()));
        std::fs::create_dir_all(r.join("usr/bin")).unwrap();
        std::fs::create_dir_all(r.join("opt/app")).unwrap();
        std::fs::write(r.join("opt/app/run"), "x").unwrap();
        std::os::unix::fs::symlink("/opt/app/run", r.join("usr/bin/run")).unwrap();
        std::os::unix::fs::symlink("/definitely/not/in/package", r.join("usr/bin/host")).unwrap();
        fix_symlinks(&r);
        assert_eq!(std::fs::read_link(r.join("usr/bin/run")).unwrap(), PathBuf::from("../../opt/app/run"));
        assert!(std::fs::read(r.join("usr/bin/run")).is_ok());
        assert_eq!(std::fs::read_link(r.join("usr/bin/host")).unwrap(), PathBuf::from("/definitely/not/in/package"));
        let _ = std::fs::remove_dir_all(r);
    }

    #[test]
    fn archive_paths_are_sanitised() {
        assert_eq!(safe_rel("./usr/bin/x"), Some(PathBuf::from("usr/bin/x")));
        assert_eq!(safe_rel("/etc/passwd"), Some(PathBuf::from("etc/passwd")));
        assert_eq!(safe_rel("../../etc/passwd"), None);
        assert_eq!(safe_rel("a/../../b"), None);
        assert_eq!(safe_rel("."), None);
    }
}
