//! Plain archives (`.tar.gz|xz|zst|bz2`, `.tgz`, `.zip`): unpack, find the main executable and
//! make it launchable. GUI-looking apps (icons/.desktop inside) get a launcher; the rest are CLI tools
//! and get a symlink in `~/.local/bin`.
use super::tree::{decompress, decompress_reader, dest, install_tree, safe_rel, Spec};
use super::{slug, Backend, Info, Opts};
use crate::desktop;
use crate::{dirs::Dirs, manifest::Manifest, pe, Error, Result};
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
    /// A tar on disk with its compression suffix, decoded as a stream (archives can be many GB).
    TarFile(PathBuf, &'static str),
    Zip(Vec<u8>),
    SevenZ(Vec<u8>),
}

fn open(path: &Path) -> Result<Data> {
    if is_zip(path) {
        return Ok(Data::Zip(std::fs::read(path)?));
    }
    if is_7z(path) {
        return Ok(Data::SevenZ(std::fs::read(path)?));
    }
    let n = lower(path);
    let name = if n.ends_with(".tgz") {
        ".tgz"
    } else if n.ends_with(".txz") || n.ends_with(".xz") {
        ".xz"
    } else if n.ends_with(".tbz2") || n.ends_with(".bz2") {
        ".bz2"
    } else if n.ends_with(".gz") {
        ".gz"
    } else if n.ends_with(".zst") {
        ".zst"
    } else {
        ".tar"
    };
    Ok(Data::TarFile(path.to_owned(), name))
}

fn tar_stream(path: &Path, name: &str) -> Result<Box<dyn Read>> {
    if name == ".xz" {
        if let Some(r) = crate::xzpar::XzPar::open(path) {
            return Ok(Box::new(std::io::BufReader::with_capacity(1 << 20, r)));
        }
    }
    decompress_reader(name, std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(path)?))
}

fn list_tar<R: Read>(mut ar: tar::Archive<R>, out: &mut Vec<Entry>) -> Result<()> {
    for e in ar.entries()? {
        crate::progress::check()?;
        let mut e = e?;
        if !e.header().entry_type().is_file() {
            continue;
        }
        let Some(p) = safe_rel(&e.path()?.to_string_lossy()) else { continue };
        let mut magic = [0u8; 4];
        let _ = e.read(&mut magic);
        out.push(Entry { path: p, mode: e.header().mode()?, size: e.header().size()?, magic });
    }
    Ok(())
}

fn list(d: &Data) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    match d {
        Data::Tar(t) => list_tar(tar::Archive::new(t.as_slice()), &mut out)?,
        Data::TarFile(p, c) => {
            crate::progress::status(t!("Читаю архів…", "Reading archive…"));
            list_tar(tar::Archive::new(tar_stream(p, c)?), &mut out)?
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
    // mode 0 = the format keeps no permissions (zip/7z made on Windows): trust the content instead
    (e.mode & 0o111 != 0 || e.mode == 0) && (&e.magic == b"\x7fELF" || &e.magic[..2] == b"#!")
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
pub(super) fn stem(path: &Path) -> String {
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
pub(super) fn version_of(s: &str) -> Option<String> {
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
    /// Command name in ~/.local/bin when the program's own file name says nothing (`start.sh`).
    bin: Option<String>,
    /// A game packed with Kron4ek's Wine wrapper (`start.sh` + `game_info/game_info.txt`).
    pack: bool,
}

fn is_elf64(p: &Path) -> bool {
    let mut h = [0u8; 5];
    std::fs::File::open(p).and_then(|mut f| f.read_exact(&mut h)).is_ok() && &h[..4] == b"\x7fELF" && h[4] == 2
}

fn is_wine_pack(entries: &[Entry], exe: &Path) -> bool {
    let dir = exe.parent().unwrap_or(Path::new(""));
    exe.extension().is_some_and(|x| x == "sh") && entries.iter().any(|e| e.path == dir.join("game_info/game_info.txt"))
}

/// Set up a "Wine wrapper" game to run through ustan's runner: system-Wine mode, a wow64 prefix when
/// the runner has no 32-bit `wine`, DXVK's d3d9 for Direct3D 9 games, and a launcher that puts the
/// runner's wrapper first in PATH. The package's own files stay as shipped otherwise.
fn setup_wine_pack(root: &Path, dirs: &Dirs, id: &str, name: &str, exe: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    use std::io::Write;
    let pkg = exe.parent().unwrap();
    let script_name = exe.file_stem().unwrap().to_string_lossy().into_owned();
    let mut settings = String::new();
    if let Ok((w, _, r)) = crate::runner::ensure(dirs) {
        settings.push_str("export USE_SYSTEM_WINE=1\n");
        if let Some(rr) = &r.root {
            if is_elf64(&rr.join("bin/wine")) {
                settings.push_str("export WINEARCH=win64\n"); // wow64 build: its `wine` is 64-bit and runs 32-bit games in a 64-bit prefix
            }
            let dll = rr.join("lib/wine/dxvk/i386-windows/d3d9.dll");
            let dlls = pkg.join("game_info/dlls");
            let wants_d3d9 = std::fs::read_dir(&dlls).map(|rd| rd.flatten().any(|e| e.file_name().to_string_lossy().to_lowercase().starts_with("d3dx9"))).unwrap_or(false);
            if dll.is_file() && wants_d3d9 && !dlls.join("d3d9.dll").exists() {
                std::fs::copy(&dll, dlls.join("d3d9.dll"))?;
            }
        }
        let _ = w;
        let sh = pkg.join("game_info/sh");
        std::fs::create_dir_all(&sh)?;
        std::fs::write(
            sh.join("10-syswow64.sh"),
            "#!/bin/sh\n# wow64 Wine loads 32-bit DLLs from syswow64\nfor x in game_info/dlls/*.dll; do\n\t[ -d \"$WINEPREFIX/drive_c/windows/syswow64\" ] && ln -sf \"$PWD/$x\" \"$WINEPREFIX/drive_c/windows/syswow64/$(basename \"$x\")\"\ndone\n",
        )?;
        std::fs::set_permissions(sh.join("10-syswow64.sh"), std::fs::Permissions::from_mode(0o755))?;
    }
    let sf = pkg.join(format!("settings_{script_name}"));
    if !settings.is_empty() && !sf.exists() {
        std::fs::write(&sf, settings)?; // start.sh sources it over its own defaults
    }
    std::fs::set_permissions(exe, std::fs::Permissions::from_mode(0o755))?;

    // wrapper: runner first in PATH
    let run = pkg.join("ustan-run.sh");
    let mut f = std::fs::File::create(&run)?;
    writeln!(f, "#!/bin/sh\nexport PATH=\"{}:$PATH\"\nexec \"{}\" \"$@\"", dirs.state.join("runner").display(), exe.display())?;
    drop(f);
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755))?;

    let game = std::fs::read_to_string(pkg.join("game_info/game_info.txt")).ok().and_then(|t| t.lines().nth(5).map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned)).unwrap_or_else(|| name.to_string());
    let icon = ["icon.png", "icon.ico", "icon"].iter().map(|n| pkg.join("game_info").join(n)).find(|p| p.is_file()).or_else(|| desktop::find_icon_deep(root, name));
    let mut t = format!("[Desktop Entry]\nType=Application\nName={game}\nExec=\"{}\"\nPath={}\nTerminal=false\nCategories=Game;\n", run.display(), pkg.display());
    if let Some(i) = icon {
        t.push_str(&format!("Icon={}\n", i.display()));
    }
    std::fs::create_dir_all(&dirs.apps)?;
    let dst = dirs.apps.join(format!("ustan-{id}.desktop"));
    std::fs::write(&dst, t)?;
    files.push(dst);

    let bin = dirs.opt.parent().unwrap_or(&dirs.opt).join("bin");
    std::fs::create_dir_all(&bin)?;
    let link = bin.join(id);
    if std::fs::symlink_metadata(&link).is_err() {
        std::os::unix::fs::symlink(&run, &link)?;
        files.push(link);
    }
    Ok(())
}

/// File names that do not name the program. A game or app shipped as `Foo/start.sh` is "Foo".
fn is_generic(file_stem: &str) -> bool {
    ["start", "run", "launch", "launcher", "startup", "play"].contains(&file_stem.to_lowercase().as_str())
}

/// Name of the program: the file's own, or (for `start.sh` and alike) the archive's top directory
/// or file name, without platform noise like `_Linux` / `-x86_64`.
fn program_name(exe: &Path, archive: &Path) -> (String, bool) {
    let own = exe.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if !is_generic(&own) {
        return (own, false);
    }
    let mut c = exe.components();
    let top = (exe.components().count() > 1).then(|| c.next().map(|x| x.as_os_str().to_string_lossy().into_owned())).flatten();
    let mut n = top.unwrap_or_else(|| stem(archive));
    let low = n.to_lowercase();
    for noise in ["_linux", "-linux", "_x86_64", "-x86_64", "_amd64", "-amd64", "_x64", "-x64", "_portable", "-portable"] {
        if let Some(i) = low.rfind(noise) {
            if i > 0 && i + noise.len() == low.len() {
                n.truncate(i);
                break;
            }
        }
    }
    (n, true)
}

fn title(name: &str) -> String {
    let mut c = name.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

fn plan(path: &Path, d: &Data) -> Result<Plan> {
    let entries = list(d)?;
    if let Some(e) = main_exe(&entries, &stem(path)) {
        let (name, generic) = program_name(&e.path, path);
        let id = slug(&name);
        return Ok(Plan { bin: generic.then(|| id.clone()), id, name: title(&name), exe: e.path.clone(), gui: looks_gui(&entries), wine: false, pack: is_wine_pack(&entries, &e.path) });
    }
    // No Linux program: a portable Windows app in a zip/7z is common, run it through Wine.
    if let Some(e) = main_win_exe(&entries, &stem(path)) {
        let name = e.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        return Ok(Plan { id: slug(&name), name, exe: e.path.clone(), gui: true, wine: true, bin: None, pack: false });
    }
    Err(Error::Format(t!("не знайшов в архіві ні виконуваного файлу для Linux, ні програми для Windows", "found neither a Linux executable nor a Windows program in the archive").into()))
}

fn unpack_tar<R: Read>(mut ar: tar::Archive<R>, root: &Path) -> Result<()> {
    ar.set_preserve_permissions(true);
    crate::progress::status(t!("Розпаковую…", "Unpacking…"));
    for e in ar.entries()? {
        crate::progress::check()?;
        e?.unpack_in(root)?;
    }
    Ok(())
}

fn extract(d: &Data, root: &Path) -> Result<()> {
    match d {
        Data::Tar(t) => unpack_tar(tar::Archive::new(t.as_slice()), root)?,
        Data::TarFile(p, c) => unpack_tar(tar::Archive::new(tar_stream(p, c)?), root)?,
        Data::SevenZ(z) => {
            let canon = root.canonicalize()?;
            let mut r = sevenz_rust::SevenZReader::new(Cursor::new(z), z.len() as u64, sevenz_rust::Password::empty()).map_err(|e| Error::Format(format!("7z: {e}")))?;
            r.for_each_entries(|e, rd| {
                if crate::progress::cancelled() {
                    return Err(sevenz_rust::Error::other(t!("скасовано", "cancelled")));
                }
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
                crate::progress::check()?;
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

/// Unpack a zip/7z/tar archive as it is into `dest`: plain extraction, nothing is installed.
pub fn extract_to(path: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    extract(&open(path)?, dest)
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
        let warning = (p.wine && !crate::runner::available(&Dirs::from_env())).then(|| t!("це програма для Windows, а Wine не знайдено (ні в PATH, ні PortProton, ні Steam Proton)", "this is a Windows program and Wine was not found (not in PATH, PortProton or Steam Proton)").to_string());
        Ok(Info { id: p.id, name: p.name, version: version_of(&stem(path)), kind: "archive", icon: None, warning })
    }

    fn install(&self, path: &Path, dirs: &Dirs, opts: &Opts) -> Result<Manifest> {
        let d = open(path)?;
        let p = plan(path, &d)?;
        let spec = Spec { kind: "archive", id: p.id.clone(), name: p.name.clone(), version: version_of(&stem(path)), source: path, overlay: false };
        install_tree(
            spec,
            dirs,
            opts,
            |root| extract(&d, root),
            |root, dirs, id, files| {
                let exe = root.join(&p.exe);
                if p.wine {
                    let wc = super::exe::wine_command(dirs);
                    let icon = std::fs::read(&exe).ok().and_then(|d| pe::icon_png(&d).ok().flatten());
                    let mut t = format!(
                        "[Desktop Entry]\nType=Application\nName={}\nExec=env \"WINEPREFIX={}\" {wc} \"{}\"\nPath={}\nCategories=Wine;\nStartupWMClass={}\n",
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
                if p.pack {
                    return setup_wine_pack(root, dirs, id, &p.name, &exe, files);
                }
                std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))?;
                if files.len() > 1 {
                    return Ok(()); // the archive shipped its own launchers (usr/share/applications)
                }
                super::tree::make_launcher(root, dirs, id, &p.name, &exe, p.gui || super::tree::is_gui_exe(&exe), p.bin.as_deref(), files)?;
                Ok(())
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_program_names_come_from_the_archive() {
        let a = Path::new("/x/NFSCarbon_Linux.tar.xz");
        assert_eq!(program_name(Path::new("NFSCarbon/RUS/start.sh"), a), ("NFSCarbon".to_string(), true));
        assert_eq!(program_name(Path::new("start.sh"), a), ("NFSCarbon".to_string(), true));
        assert_eq!(program_name(Path::new("bin/rg"), a), ("rg".to_string(), false));
    }

    #[test]
    fn extract_to_unpacks_a_zip_of_plain_files() {
        let d = std::env::temp_dir().join(format!("ustan-xt-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let z = d.join("photos.zip");
        let mut w = zip::ZipWriter::new(std::fs::File::create(&z).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        w.start_file("a/readme.txt", o).unwrap();
        std::io::Write::write_all(&mut w, b"hi").unwrap();
        w.start_file("../escape.txt", o).unwrap(); // must not leave the target
        std::io::Write::write_all(&mut w, b"no").unwrap();
        w.finish().unwrap();
        extract_to(&z, &d.join("out")).unwrap();
        assert_eq!(std::fs::read(d.join("out/a/readme.txt")).unwrap(), b"hi");
        assert!(!d.join("escape.txt").exists());
        let _ = std::fs::remove_dir_all(d);
    }

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
    fn programs_in_archives_without_permissions_are_found_by_content() {
        let e = |p: &str, mode: u32, magic: &[u8; 4]| Entry { path: PathBuf::from(p), mode, size: 1, magic: *magic };
        assert!(is_program(&e("a/tool", 0, b"\x7fELF")), "no mode info + ELF magic");
        assert!(is_program(&e("a/run.sh", 0, b"#!/b")));
        assert!(!is_program(&e("a/README", 0, b"# Re")));
        assert!(!is_program(&e("a/data.bin", 0o644, b"\x7fELF")), "an explicit non-executable mode still wins");
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


// ---------- makeself (.run / .sh self-extracting archives) ----------

/// A shell script followed by a tar payload. We only *read* the payload; the script is never run.
pub struct Makeself;

/// Payload of a makeself archive: everything after the first `skip` lines of the header.
fn makeself_payload(bytes: &[u8]) -> Result<&[u8]> {
    let head = &bytes[..bytes.len().min(16 * 1024)];
    let text = String::from_utf8_lossy(head);
    if !text.contains("Makeself") {
        return Err(Error::Format(t!("це не makeself-архів", "not a makeself archive").into()));
    }
    let skip: usize = text
        .lines()
        .find_map(|l| l.strip_prefix("skip=").or_else(|| l.strip_prefix("SKIP=")))
        .and_then(|v| v.trim().trim_matches(['"', '\'']).parse().ok())
        .ok_or_else(|| Error::Format("у заголовку makeself немає skip=".into()))?;
    let mut off = 0usize;
    for _ in 0..skip {
        off += bytes[off..].iter().position(|b| *b == b'\n').ok_or_else(|| Error::Format("заголовок makeself обірваний".into()))? + 1;
    }
    Ok(&bytes[off..])
}

fn makeself_tar(path: &Path) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path)?;
    let payload = makeself_payload(&bytes)?;
    let ext = match payload {
        [0x1f, 0x8b, ..] => ".gz",
        [b'B', b'Z', b'h', ..] => ".bz2",
        [0xfd, b'7', b'z', b'X', b'Z', 0, ..] => ".xz",
        [0x28, 0xb5, 0x2f, 0xfd, ..] => ".zst",
        _ => ".tar",
    };
    decompress(ext, payload)
}

impl Backend for Makeself {
    fn kind(&self) -> &'static str {
        "makeself"
    }

    fn detect(&self, path: &Path) -> bool {
        let n = lower(path);
        (n.ends_with(".run") || n.ends_with(".sh") || n.ends_with(".bin"))
            && std::fs::File::open(path)
                .and_then(|mut f| {
                    let mut b = vec![0u8; 4096];
                    let n = f.read(&mut b)?;
                    Ok(b[..n].windows(8).any(|w| w == b"Makeself"))
                })
                .unwrap_or(false)
    }

    fn inspect(&self, path: &Path) -> Result<Info> {
        let d = Data::Tar(makeself_tar(path)?);
        plan(path, &d)?; // fails when there is no program inside
        let st = stem_of_script(path);
        let name = super::tree::clean_name(&st);
        let warning = Some("скрипти встановлення з .run не виконуються: файли лише розпаковуються".to_string());
        Ok(Info { id: slug(&name), name, version: version_of(&st), kind: "makeself", icon: None, warning })
    }

    fn install(&self, path: &Path, dirs: &Dirs, opts: &Opts) -> Result<Manifest> {
        let d = Data::Tar(makeself_tar(path)?);
        let p = plan(path, &d)?;
        let name = super::tree::clean_name(&stem_of_script(path));
        let spec = Spec { kind: "makeself", id: slug(&name), name: name.clone(), version: version_of(&stem_of_script(path)), source: path, overlay: false };
        install_tree(
            spec,
            dirs,
            opts,
            |root| extract(&d, root),
            |root, dirs, id, files| {
                let exe = root.join(&p.exe);
                std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))?;
                super::tree::make_launcher(root, dirs, id, &p.name, &exe, p.gui || super::tree::is_gui_exe(&exe), None, files)
            },
        )
    }
}

fn stem_of_script(path: &Path) -> String {
    let n = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    for e in [".run", ".sh", ".bin"] {
        if let Some(s) = n.strip_suffix(e) {
            return s.to_string();
        }
    }
    n
}

#[cfg(test)]
mod makeself_tests {
    use super::*;

    #[test]
    fn payload_starts_after_the_header_lines() {
        let mut f = b"#!/bin/sh\n# This script was generated using Makeself 2.5\nskip=\"4\"\nexit 0\nPAYLOAD".to_vec();
        assert_eq!(makeself_payload(&f).unwrap(), b"PAYLOAD");
        f[0] = b'x';
        assert!(makeself_payload(b"#!/bin/sh\necho hi\n").is_err(), "a plain script is not a makeself archive");
        assert!(makeself_payload(b"# Makeself\nnothing here\n").is_err());
    }
}
