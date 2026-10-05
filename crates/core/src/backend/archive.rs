//! Plain archives (`.tar.gz|xz|zst|bz2`, `.tgz`, `.zip`): unpack, find the main executable and
//! make it launchable. GUI-looking apps (icons/.desktop inside) get a launcher; the rest are CLI tools
//! and get a symlink in `~/.local/bin`.
use super::tree::{decompress, dest, install_tree, safe_rel, which, Spec};
use super::{slug, Backend, Info, Opts};
use crate::{desktop, dirs::Dirs, manifest::Manifest, pe, Error, Result};
use std::io::{Cursor, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub struct Archive;

const TAR_EXTS: &[&str] = &[".tar.gz", ".tgz", ".tar.xz", ".txz", ".tar.zst", ".tar.bz2", ".tbz2", ".tar"];

fn lower(path: &Path) -> String {
    path.file_name().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn is_zip(path: &Path) -> bool {
    lower(path).ends_with(".zip")
}

fn is_7z(path: &Path) -> bool {
    lower(path).ends_with(".7z")
}

fn is_tar(path: &Path) -> bool {
    let n = lower(path);
    !n.contains(".pkg.tar") && TAR_EXTS.iter().any(|e| n.ends_with(e))
}

/// One regular file inside an archive.
struct Entry {
    path: PathBuf,
    mode: u32,
    size: u64,
    magic: [u8; 4],
}

enum Data {
    Tar(Vec<u8>),
    Zip(Vec<u8>),
    SevenZ(Vec<u8>),
}

fn open(path: &Path) -> Result<Data> {
    let bytes = std::fs::read(path)?;
    if is_zip(path) {
        return Ok(Data::Zip(bytes));
    }
    if is_7z(path) {
        return Ok(Data::SevenZ(bytes));
    }
    let n = lower(path);
    let name = if n.ends_with(".tgz") { ".tgz" } else if n.ends_with(".txz") { ".xz" } else if n.ends_with(".tbz2") { ".bz2" } else { &n };
    Ok(Data::Tar(decompress(name, &bytes)?))
}

fn list(d: &Data) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    match d {
        Data::Tar(t) => {
            for e in tar::Archive::new(t.as_slice()).entries()? {
                let mut e = e?;
                if !e.header().entry_type().is_file() {
                    continue;
                }
                let Some(p) = safe_rel(&e.path()?.to_string_lossy()) else { continue };
                let mut magic = [0u8; 4];
                let _ = e.read(&mut magic);
                out.push(Entry { path: p, mode: e.header().mode()?, size: e.header().size()?, magic });
            }
        }
        Data::SevenZ(z) => {
            let mut r = sevenz_rust::SevenZReader::new(Cursor::new(z), z.len() as u64, sevenz_rust::Password::empty()).map_err(|e| Error::Format(format!("7z: {e}")))?;
            r.for_each_entries(|e, rd| {
                if !e.is_directory() && e.has_stream() {
                    if let Some(p) = safe_rel(e.name()) {
                        let mut magic = [0u8; 4];
                        let _ = rd.read(&mut magic);
                        let a = e.windows_attributes();
                        out.push(Entry { path: p, mode: if a & 0x8000 != 0 { a >> 16 } else { 0 }, size: e.size(), magic });
                    }
                }
                // the stream must be consumed before the next entry
                std::io::copy(rd, &mut std::io::sink())?;
                Ok(true)
            })
            .map_err(|e| Error::Format(format!("7z: {e}")))?;
        }
        Data::Zip(z) => {
            let mut zr = zip::ZipArchive::new(Cursor::new(z)).map_err(|e| Error::Format(format!("zip: {e}")))?;
            for i in 0..zr.len() {
                let mut f = zr.by_index(i).map_err(|e| Error::Format(format!("zip: {e}")))?;
                if !f.is_file() {
                    continue;
                }
                let Some(p) = f.enclosed_name() else { continue };
                let mut magic = [0u8; 4];
                let _ = f.read(&mut magic);
                out.push(Entry { path: p, mode: f.unix_mode().unwrap_or(0), size: f.size(), magic });
            }
        }
    }
    Ok(out)
}

fn is_program(e: &Entry) -> bool {
    e.mode & 0o111 != 0 && (&e.magic == b"\x7fELF" || &e.magic[..2] == b"#!")
}

/// Pick the entry most likely to be "the" program of the archive.
fn main_exe<'a>(entries: &'a [Entry], stem: &str) -> Option<&'a Entry> {
    let stem = stem.to_lowercase();
    entries
        .iter()
        .filter(|e| is_program(e))
        .filter(|e| {
            // shared libraries and helper dirs are not programs
            let s = e.path.to_string_lossy();
            !s.contains(".so") && !s.contains("/lib/") && !s.contains("/libexec/") && !s.starts_with("lib/")
        })
        .max_by_key(|e| {
            let fname = e.path.file_name().unwrap().to_string_lossy().to_lowercase();
            let depth = e.path.components().count() as i64;
            let mut score = -depth * 10;
            if e.path.components().any(|c| c.as_os_str() == "bin") {
                score += 15;
            }
            if !fname.is_empty() && stem.contains(&fname) {
                score += 40;
            }
            (score, e.size as i64)
        })
}

fn looks_gui(entries: &[Entry]) -> bool {
    entries.iter().any(|e| {
        let n = e.path.to_string_lossy().to_lowercase();
        n.ends_with(".desktop") || n.ends_with(".svg") || n.ends_with(".png") && (n.contains("icon") || n.contains("logo"))
    })
}

/// File name without any archive extension, e.g. `ripgrep-14.1-x86_64.tar.gz` -> `ripgrep-14.1-x86_64`.
fn stem(path: &Path) -> String {
    let n = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let l = n.to_lowercase();
    for e in TAR_EXTS.iter().chain(&[".zip", ".7z"]) {
        if l.ends_with(e) {
            return n[..n.len() - e.len()].to_string();
        }
    }
    n
}

/// The main Windows program of an archive (for Wine): a PE file, not an uninstaller/redistributable.
fn main_win_exe<'a>(entries: &'a [Entry], stem: &str) -> Option<&'a Entry> {
    let stem = stem.to_lowercase();
    entries
        .iter()
        .filter(|e| e.path.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe")) && &e.magic[..2] == b"MZ")
        .filter(|e| {
            let n = e.path.file_name().unwrap().to_string_lossy().to_lowercase();
            !["unins", "uninst", "vcredist", "vc_redist", "dxsetup", "crashpad", "crashhandler", "dotnet", "directx"].iter().any(|b| n.contains(b))
        })
        .max_by_key(|e| {
            let fname = e.path.file_stem().unwrap().to_string_lossy().to_lowercase();
            let mut score = -(e.path.components().count() as i64) * 10;
            if !fname.is_empty() && stem.contains(&fname) {
                score += 40;
            }
            (score, e.size as i64)
        })
}

/// First `1.2` / `1.2.3` style number in a name.
fn version_of(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let boundary = i == 0
            || !b[i - 1].is_ascii_alphanumeric()
            || (matches!(b[i - 1], b'v' | b'V') && (i == 1 || !b[i - 2].is_ascii_alphanumeric()));
        if b[i].is_ascii_digit() && boundary {
            let mut j = i;
            let mut dots = 0;
            while j < b.len() && (b[j].is_ascii_digit() || (b[j] == b'.' && j + 1 < b.len() && b[j + 1].is_ascii_digit())) {
                dots += (b[j] == b'.') as u32;
                j += 1;
            }
            if dots >= 1 {
                return Some(s[i..j].to_string());
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

struct Plan {
    id: String,
    name: String,
    exe: PathBuf,
    gui: bool,
    /// A Windows program: launched through Wine.
    wine: bool,
}

fn title(name: &str) -> String {
    let mut c = name.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

fn plan(path: &Path, d: &Data) -> Result<Plan> {
    let entries = list(d)?;
    if let Some(e) = main_exe(&entries, &stem(path)) {
        let name = e.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        return Ok(Plan { id: slug(&name), name: title(&name), exe: e.path.clone(), gui: looks_gui(&entries), wine: false });
    }
    // No Linux program: a portable Windows app in a zip/7z is common, run it through Wine.
    if let Some(e) = main_win_exe(&entries, &stem(path)) {
        let name = e.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        return Ok(Plan { id: slug(&name), name, exe: e.path.clone(), gui: true, wine: true });
    }
    Err(Error::Format("не знайшов в архіві ні виконуваного файлу для Linux, ні програми для Windows".into()))
}

fn extract(d: &Data, root: &Path) -> Result<()> {
    match d {
        Data::Tar(t) => {
            let mut ar = tar::Archive::new(t.as_slice());
            ar.set_preserve_permissions(true);
            for e in ar.entries()? {
                e?.unpack_in(root)?;
            }
        }
        Data::SevenZ(z) => {
            let canon = root.canonicalize()?;
            let mut r = sevenz_rust::SevenZReader::new(Cursor::new(z), z.len() as u64, sevenz_rust::Password::empty()).map_err(|e| Error::Format(format!("7z: {e}")))?;
            r.for_each_entries(|e, rd| {
                let Some(rel) = safe_rel(e.name()) else {
                    std::io::copy(rd, &mut std::io::sink())?;
                    return Ok(true);
                };
                let d = dest(root, &canon, &rel).map_err(|x| sevenz_rust::Error::other(x.to_string()))?;
                if e.is_directory() {
                    std::fs::create_dir_all(&d)?;
                } else {
                    let mut out = std::fs::File::create(&d)?;
                    std::io::copy(rd, &mut out)?;
                    let a = e.windows_attributes();
                    if a & 0x8000 != 0 {
                        std::fs::set_permissions(&d, std::fs::Permissions::from_mode((a >> 16) & 0o777))?;
                    }
                }
                Ok(true)
            })
            .map_err(|e| Error::Format(format!("7z: {e}")))?;
        }
        Data::Zip(z) => {
            let mut zr = zip::ZipArchive::new(Cursor::new(z)).map_err(|e| Error::Format(format!("zip: {e}")))?;
            for i in 0..zr.len() {
                let mut f = zr.by_index(i).map_err(|e| Error::Format(format!("zip: {e}")))?;
                let Some(rel) = f.enclosed_name() else { continue };
                let dst = root.join(rel);
                if f.is_dir() {
                    std::fs::create_dir_all(&dst)?;
                    continue;
                }
                if let Some(p) = dst.parent() {
                    std::fs::create_dir_all(p)?;
                }
                let mut out = std::fs::File::create(&dst)?;
                std::io::copy(&mut f, &mut out)?;
                if let Some(m) = f.unix_mode() {
                    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(m & 0o777))?;
                }
            }
        }
    }
    Ok(())
}

impl Backend for Archive {
    fn kind(&self) -> &'static str {
        "archive"
    }

    fn detect(&self, path: &Path) -> bool {
        is_zip(path) || is_7z(path) || is_tar(path)
    }

    fn inspect(&self, path: &Path) -> Result<Info> {
        let d = open(path)?;
        let p = plan(path, &d)?;
        let warning = (p.wine && which("wine").is_none()).then(|| "це програма для Windows, потрібен wine (його немає в PATH)".to_string());
        Ok(Info { id: p.id, name: p.name, version: version_of(&stem(path)), kind: "archive", icon: None, warning })
    }

    fn install(&self, path: &Path, dirs: &Dirs, opts: &Opts) -> Result<Manifest> {
        let d = open(path)?;
        let p = plan(path, &d)?;
        let spec = Spec { kind: "archive", id: p.id.clone(), name: p.name.clone(), version: version_of(&stem(path)), source: path };
        install_tree(
            spec,
            dirs,
            opts,
            |root| extract(&d, root),
            |root, dirs, id, files| {
                let exe = root.join(&p.exe);
                if p.wine {
                    let icon = std::fs::read(&exe).ok().and_then(|d| pe::icon_png(&d).ok().flatten());
                    let mut t = format!(
                        "[Desktop Entry]\nType=Application\nName={}\nExec=env \"WINEPREFIX={}\" wine \"{}\"\nPath={}\nCategories=Wine;\nStartupWMClass={}\n",
                        p.name,
                        root.join("prefix").display(),
                        exe.display(),
                        exe.parent().unwrap().display(),
                        exe.file_name().unwrap().to_string_lossy().to_lowercase()
                    );
                    if let Some(png) = icon {
                        let ip = root.join("icon.png");
                        std::fs::write(&ip, png)?;
                        t.push_str(&format!("Icon={}\n", ip.display()));
                    }
                    let dst = dirs.apps.join(format!("ustan-{id}.desktop"));
                    std::fs::write(&dst, t)?;
                    files.push(dst);
                    return Ok(());
                }
                std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))?;
                if files.len() > 1 {
                    return Ok(()); // the archive shipped its own launchers (usr/share/applications)
                }
                if p.gui {
                    let icon = desktop::find_icon_deep(root, &p.name);
                    let mut t = format!(
                        "[Desktop Entry]\nType=Application\nName={}\nExec=\"{}\"\nPath={}\nTerminal=false\nCategories=Utility;\n",
                        p.name,
                        exe.display(),
                        exe.parent().unwrap().display()
                    );
                    if let Some(i) = icon {
                        t.push_str(&format!("Icon={}\n", i.display()));
                    }
                    let dst = dirs.apps.join(format!("ustan-{id}.desktop"));
                    std::fs::write(&dst, t)?;
                    files.push(dst);
                } else {
                    // A command-line tool: make it available in PATH.
                    let bin = dirs.opt.parent().unwrap_or(&dirs.opt).join("bin");
                    std::fs::create_dir_all(&bin)?;
                    let link = bin.join(exe.file_name().unwrap());
                    if std::fs::symlink_metadata(&link).is_ok() {
                        return Err(Error::Format(format!("{} уже існує в {}", link.file_name().unwrap().to_string_lossy(), bin.display())));
                    }
                    std::os::unix::fs::symlink(&exe, &link)?;
                    files.push(link);
                }
                Ok(())
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_versions() {
        assert_eq!(stem(Path::new("/d/ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz")), "ripgrep-14.1.0-x86_64-unknown-linux-musl");
        assert_eq!(stem(Path::new("Tool_1.2.TGZ")), "Tool_1.2");
        assert_eq!(version_of("ripgrep-14.1.0-x86_64"), Some("14.1.0".into()));
        assert_eq!(version_of("app-v2.5"), Some("2.5".into()));
        assert_eq!(version_of("x86_64"), None);
        assert!(is_tar(Path::new("a.tar.zst")) && !is_tar(Path::new("a.pkg.tar.zst")));
    }

    #[test]
    fn windows_main_exe_skips_uninstallers() {
        let e = |p: &str, size: u64, magic: &[u8; 4]| Entry { path: PathBuf::from(p), mode: 0, size, magic: *magic };
        let v = vec![
            e("App/unins000.exe", 9_000_000, b"MZ\x90\x00"),
            e("App/vc_redist.x64.exe", 20_000_000, b"MZ\x90\x00"),
            e("App/App.exe", 1_000_000, b"MZ\x90\x00"),
            e("App/notes.exe", 10, b"junk"),
            e("App/bin/helper.exe", 500, b"MZ\x90\x00"),
        ];
        assert_eq!(main_win_exe(&v, "App-1.0").unwrap().path, PathBuf::from("App/App.exe"));
        assert!(main_win_exe(&[e("x/readme.txt", 5, b"hi!!")], "x").is_none());
    }

    #[test]
    fn picks_the_program_not_the_library() {
        let e = |p: &str, mode: u32, size: u64, magic: &[u8; 4]| Entry { path: PathBuf::from(p), mode, size, magic: *magic };
        let v = vec![
            e("app-1.0/lib/libfoo.so", 0o755, 9_000_000, b"\x7fELF"),
            e("app-1.0/README", 0o644, 100, b"# Ap"),
            e("app-1.0/bin/app", 0o755, 500_000, b"\x7fELF"),
            e("app-1.0/bin/helper", 0o755, 900_000, b"\x7fELF"),
        ];
        assert_eq!(main_exe(&v, "app-1.0").unwrap().path, PathBuf::from("app-1.0/bin/app"));
    }
}
