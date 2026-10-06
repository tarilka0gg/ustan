//! Shared logic for packages that are unpacked into a private root (`~/.local/opt/<id>`):
//! deb, rpm, Arch packages and plain archives.
use super::Opts;
use crate::{desktop, dirs::Dirs, manifest::Manifest, Error, Result};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub struct Spec<'a> {
    pub kind: &'static str,
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub source: &'a Path,
    /// Run the package's programs with its `usr` (and `opt`) overlaid on the host's, so absolute
    /// paths such as /usr/share/<app> resolve (needs bubblewrap; without it programs run unwrapped).
    pub overlay: bool,
}

/// Does this shared library mean the program has a graphical interface?
pub fn is_gui_lib(n: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "libgtk-", "libadwaita", "libQt5Widgets", "libQt5Gui", "libQt5Quick", "libQt6Widgets", "libQt6Gui", "libQt6Quick",
        "libwayland-client", "libX11.so", "libxcb.so", "libSDL2", "libSDL3", "libglfw", "libwx_", "libfltk", "libraylib", "libgdk-",
    ];
    PREFIXES.iter().any(|p| n.starts_with(p))
}

/// Is `path` an ELF program that links a GUI toolkit?
pub fn is_gui_exe(path: &Path) -> bool {
    crate::elf::needed_of_file(path).map(|n| n.iter().any(|l| is_gui_lib(l))).unwrap_or(false)
}

fn host_libs() -> &'static std::collections::HashSet<String> {
    static LIBS: std::sync::OnceLock<std::collections::HashSet<String>> = std::sync::OnceLock::new();
    LIBS.get_or_init(|| {
        let mut set = std::collections::HashSet::new();
        // `ldconfig -p` knows every library the dynamic linker will find
        for bin in ["ldconfig", "/sbin/ldconfig", "/usr/sbin/ldconfig"] {
            if let Ok(o) = std::process::Command::new(bin).arg("-p").output() {
                if o.status.success() {
                    for l in String::from_utf8_lossy(&o.stdout).lines() {
                        if l.contains("x86-64") {
                            if let Some(n) = l.split_whitespace().next() {
                                set.insert(n.to_string());
                            }
                        }
                    }
                    break;
                }
            }
        }
        for d in ["/usr/lib64", "/usr/lib", "/lib64", "/lib", "/usr/lib/x86_64-linux-gnu"] {
            if let Ok(rd) = std::fs::read_dir(d) {
                set.extend(rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.contains(".so")));
            }
        }
        set
    })
}

fn collect_so(dir: &Path, depth: u32, out: &mut std::collections::HashSet<String>) {
    if depth == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            collect_so(&p, depth - 1, out);
        } else if let Some(n) = p.file_name().map(|n| n.to_string_lossy().into_owned()) {
            if n.contains(".so") {
                out.insert(n);
            }
        }
    }
}

fn walk_elf_programs(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if depth == 0 || out.len() >= 40 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            walk_elf_programs(&p, depth - 1, out);
        } else if ft.is_file() && std::fs::metadata(&p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0) {
            if std::fs::File::open(&p).and_then(|mut f| { let mut b = [0u8; 4]; f.read_exact(&mut b).map(|_| b) }).is_ok_and(|b| &b == b"\x7fELF") {
                out.push(p);
            }
        }
    }
}

/// ELF executables of a package tree: `usr/bin`, `bin`, `usr/games` and the top of `opt/*`.
/// Plain archives have no fixed layout, so for them the whole tree is searched.
fn elf_programs(root: &Path, whole_tree: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if whole_tree {
        walk_elf_programs(root, 6, &mut out);
        return out;
    }
    let mut dirs: Vec<PathBuf> = ["usr/bin", "usr/games", "bin", "usr/sbin"].iter().map(|d| root.join(d)).collect();
    if let Ok(rd) = std::fs::read_dir(root.join("opt")) {
        for e in rd.flatten() {
            dirs.push(e.path());
            if let Ok(rd2) = std::fs::read_dir(e.path()) {
                dirs.extend(rd2.flatten().map(|x| x.path()).filter(|p| p.is_dir()));
            }
        }
    }
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(m) = std::fs::metadata(&p) else { continue }; // follows symlinks
            if m.is_file() && m.permissions().mode() & 0o111 != 0 && std::fs::File::open(&p).and_then(|mut f| { let mut b = [0u8; 4]; f.read_exact(&mut b).map(|_| b) }).is_ok_and(|b| &b == b"\x7fELF") {
                out.push(p);
                if out.len() >= 40 {
                    return out;
                }
            }
        }
    }
    out
}

/// Libraries the package's programs need that neither the package nor the system provides.
pub fn scan_missing(root: &Path, whole_tree: bool) -> Vec<String> {
    let mut have = host_libs().clone();
    collect_so(root, 8, &mut have);
    let mut missing = std::collections::BTreeSet::new();
    for exe in elf_programs(root, whole_tree) {
        for lib in crate::elf::needed_of_file(&exe).unwrap_or_default() {
            if !have.contains(&lib) && !lib.starts_with("ld-linux") && !lib.starts_with("linux-vdso") {
                missing.insert(lib);
            }
        }
    }
    missing.into_iter().collect()
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
        liblzma::read::XzDecoder::new(bytes).read_to_end(&mut out).map_err(|e| Error::Format(format!("xz: {e}")))?;
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

/// `.ustan-run/overlay`: runs a command as if the package were installed in `/usr` and `/opt`.
/// The *last* `--overlay-src` is the top layer, so the package goes after the host directory.
fn overlay_script(root: &Path) -> String {
    format!(
        r#"#!/bin/sh
# generated by ustan: run a package as if it were installed under /usr (and /opt)
ROOT="{root}"
if command -v bwrap >/dev/null 2>&1; then
  OPT=""
  if [ -d "$ROOT/opt" ] && [ -d /opt ]; then OPT="--overlay-src /opt --overlay-src $ROOT/opt --ro-overlay /opt"; fi
  # shellcheck disable=SC2086
  set -- bwrap --die-with-parent --dev-bind / / --overlay-src /usr --overlay-src "$ROOT/usr" --ro-overlay /usr $OPT -- "$@"
fi
exec "$@"
"#,
        root = root.display()
    )
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
        let launcher = if spec.overlay && root.join("usr").is_dir() {
            let run = root.join(".ustan-run");
            std::fs::create_dir_all(&run)?;
            let l = run.join("overlay");
            std::fs::write(&l, overlay_script(&root))?;
            std::fs::set_permissions(&l, std::fs::Permissions::from_mode(0o755))?;
            Some(l)
        } else {
            None
        };
        let apps = root.join("usr/share/applications");
        if apps.is_dir() {
            for e in std::fs::read_dir(&apps)? {
                let p = e?.path();
                if p.extension().is_some_and(|x| x == "desktop") {
                    let text = std::fs::read_to_string(&p)?;
                    let dst = dirs.apps.join(format!("ustan-{}-{}", spec.id, p.file_name().unwrap().to_string_lossy()));
                    std::fs::write(&dst, desktop::rewrite(&text, &root, launcher.as_deref()))?;
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
    let mut notes = Vec::new();
    if matches!(spec.kind, "deb" | "rpm" | "arch" | "archive") {
        let missing = scan_missing(&root, spec.kind == "archive");
        if !missing.is_empty() {
            let shown: Vec<_> = missing.iter().take(8).cloned().collect();
            let more = if missing.len() > shown.len() { format!(" та ще {}", missing.len() - shown.len()) } else { String::new() };
            notes.push(format!("Не вистачає бібліотек: {}{more}. Програма може не запуститися.", shown.join(", ")));
        }
    }
    let m = Manifest {
        id: spec.id,
        name: spec.name,
        version: spec.version,
        kind: spec.kind.into(),
        source: Some(spec.source.display().to_string()),
        files,
        notes,
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

#[cfg(test)]
mod overlay_tests {
    use super::*;

    #[test]
    fn overlay_script_puts_the_package_on_top() {
        let s = overlay_script(Path::new("/h/.local/opt/app"));
        let host = s.find("--overlay-src /usr ").unwrap();
        let pkg = s.find("--overlay-src \"$ROOT/usr\"").unwrap();
        assert!(host < pkg, "package layer must come last (top): {s}");
        assert!(s.contains("exec \"$@\""), "falls back to a plain exec without bwrap");
    }
}
