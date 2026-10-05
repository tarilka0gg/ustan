use super::{slug, Backend, Icon, Info, Opts};
use crate::{desktop, dirs::Dirs, elf, manifest::Manifest, Error, Result};
use backhand::{FilesystemReader, InnerNode, Node, SquashfsFileReader};
use std::io::{BufReader, Cursor, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub struct AppImage;

type Fs<'a> = FilesystemReader<'a>;

fn open(data: &[u8]) -> Result<Fs<'_>> {
    open_reader(data, BufReader::new(Cursor::new(data)))
}

/// `prefix` is the start of the file (enough to cover the ELF part); `reader` reads the whole file.
fn open_reader<'a>(prefix: &[u8], reader: impl backhand::BufReadSeek + 'a) -> Result<Fs<'a>> {
    if prefix.len() < 12 || &prefix[8..11] != b"AI\x02" {
        return Err(Error::Format("not an AppImage type 2".into()));
    }
    let off = elf::end(prefix)?;
    if prefix.get(off as usize..off as usize + 4) != Some(b"hsqs") {
        return Err(Error::Format("no squashfs after ELF".into()));
    }
    FilesystemReader::from_reader_with_offset(reader, off).map_err(|e| Error::Format(format!("squashfs: {e}")))
}

/// What we can learn about an AppImage on disk without loading all of it into memory.
#[derive(Debug, Clone)]
pub struct Probe {
    pub name: String,
    pub version: Option<String>,
    pub update_info: Option<String>,
}

/// Cheap magic check: ELF + `AI\x02` at offset 8.
pub fn is_appimage(path: &Path) -> bool {
    use std::io::Read as _;
    let mut b = [0u8; 12];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut b)).is_ok() && &b[..4] == b"\x7fELF" && &b[8..11] == b"AI\x02"
}

pub fn probe(path: &Path) -> Result<Probe> {
    use std::io::Read as _;
    let f = std::fs::File::open(path)?;
    let mut prefix = Vec::new();
    f.try_clone()?.take(8 << 20).read_to_end(&mut prefix)?;
    let fs = open_reader(&prefix, BufReader::new(f))?;
    let (rel, text) = top_level_desktop(&fs).ok_or_else(|| Error::Format("no .desktop in AppImage".into()))?;
    Ok(Probe {
        name: desktop::name(&text).unwrap_or_else(|| rel.trim_end_matches(".desktop").to_string()),
        version: value(&text, "X-AppImage-Version").map(str::to_string),
        update_info: update_info(&prefix),
    })
}

fn node<'a, 'b>(fs: &'a Fs<'b>, path: &str) -> Option<&'a Node<SquashfsFileReader>> {
    let want = Path::new("/").join(path);
    fs.files().find(|n| n.fullpath == want)
}

/// Read a file from the image, following up to 8 symlinks.
fn read(fs: &Fs<'_>, path: &str) -> Option<Vec<u8>> {
    let mut cur = PathBuf::from(path);
    for _ in 0..8 {
        let n = node(fs, &cur.to_string_lossy())?;
        match &n.inner {
            InnerNode::File(f) => {
                let mut v = Vec::new();
                fs.file(f).reader().read_to_end(&mut v).ok()?;
                return Some(v);
            }
            InnerNode::Symlink(l) => {
                cur = if l.link.is_absolute() { l.link.strip_prefix("/").ok()?.to_path_buf() } else { cur.parent().unwrap_or(Path::new("")).join(&l.link) };
            }
            _ => return None,
        }
    }
    None
}

fn top_level_desktop(fs: &Fs<'_>) -> Option<(String, String)> {
    let n = fs.files().find(|n| {
        n.fullpath.parent() == Some(Path::new("/")) && n.fullpath.extension().is_some_and(|e| e == "desktop")
    })?;
    let rel = n.fullpath.strip_prefix("/").ok()?.to_string_lossy().into_owned();
    let text = String::from_utf8(read(fs, &rel)?).ok()?;
    Some((rel, text))
}

/// Icon= name -> top-level <name>.{svg,png,xpm}, else .DirIcon.
fn read_icon(fs: &Fs<'_>, text: &str) -> Option<Icon> {
    let icon = value(text, "Icon").unwrap_or("");
    let cands = ["svg", "png", "xpm"].iter().map(|e| format!("{icon}.{e}")).chain([".DirIcon".to_string()]);
    for c in cands.filter(|c| !c.starts_with('.') || c == ".DirIcon") {
        if let Some(bytes) = read(fs, &c) {
            let ext = if bytes.starts_with(b"\x89PNG") { "png" } else if bytes.windows(4).take(256).any(|w| w == b"<svg") { "svg" } else { "xpm" };
            return Some(Icon { ext, bytes });
        }
    }
    None
}

/// `gh-releases-zsync|owner|repo|tag|pattern` etc. from the ELF `.upd_info` section, if present.
pub fn update_info(data: &[u8]) -> Option<String> {
    let r = elf::section(data, ".upd_info")?;
    let raw = data.get(r)?;
    let s = String::from_utf8_lossy(raw.split(|b| *b == 0).next()?).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')).map(str::trim)
}

impl Backend for AppImage {
    fn kind(&self) -> &'static str {
        "appimage"
    }

    fn detect(&self, path: &Path) -> bool {
        path.extension().is_some_and(|e| e.eq_ignore_ascii_case("appimage"))
    }

    fn inspect(&self, path: &Path) -> Result<Info> {
        let data = std::fs::read(path)?;
        let fs = open(&data)?;
        let (rel, text) = top_level_desktop(&fs).ok_or_else(|| Error::Format("no .desktop in AppImage".into()))?;
        let name = desktop::name(&text).unwrap_or_else(|| rel.trim_end_matches(".desktop").to_string());
        let icon = read_icon(&fs, &text);
        Ok(Info { id: slug(&name), name, version: value(&text, "X-AppImage-Version").map(str::to_string), kind: "appimage", icon, warning: None })
    }

    fn install(&self, path: &Path, dirs: &Dirs, _opts: &Opts) -> Result<Manifest> {
        let data = std::fs::read(path)?;
        let fs = open(&data)?;
        let (_, text) = top_level_desktop(&fs).ok_or_else(|| Error::Format("no .desktop in AppImage".into()))?;
        let name = desktop::name(&text).unwrap_or_else(|| "appimage".into());
        let id = slug(&name);
        let root = dirs.opt.join(&id);
        if root.exists() {
            return Err(Error::Format(format!("`{id}` is already installed")));
        }
        std::fs::create_dir_all(&root)?;
        let mut files = vec![root.clone()];

        let res = (|| -> Result<()> {
            let bin = root.join(path.file_name().ok_or_else(|| Error::Format("bad file name".into()))?);
            std::fs::write(&bin, &data)?;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))?;

            let icon_path = read_icon(&fs, &text).map(|i| -> Result<PathBuf> {
                let p = root.join(format!("icon.{}", i.ext));
                std::fs::write(&p, i.bytes)?;
                Ok(p)
            }).transpose()?;

            let mut out = String::new();
            for l in text.lines() {
                match l.split_once('=') {
                    Some(("Exec", v)) => {
                        let args = v.split_once(char::is_whitespace).map_or("", |(_, a)| a);
                        out.push_str(&format!("Exec={} {}\n", bin.display(), args).replace(" \n", "\n"));
                    }
                    Some(("TryExec", _)) | Some(("Path", _)) | Some(("DBusActivatable", _)) => {}
                    Some(("Icon", _)) if icon_path.is_some() => out.push_str(&format!("Icon={}\n", icon_path.as_ref().unwrap().display())),
                    _ => {
                        out.push_str(l);
                        out.push('\n');
                    }
                }
            }
            std::fs::create_dir_all(&dirs.apps)?;
            let dst = dirs.apps.join(format!("ustan-{id}.desktop"));
            std::fs::write(&dst, out)?;
            files.push(dst);
            Ok(())
        })();
        if let Err(e) = res {
            for f in files.iter().rev() {
                let _ = if f.is_dir() { std::fs::remove_dir_all(f) } else { std::fs::remove_file(f) };
            }
            return Err(e);
        }
        let update_info = update_info(&data);
        let m = Manifest { update_info, id, name, version: value(&text, "X-AppImage-Version").map(str::to_string), kind: "appimage".into(), source: Some(path.display().to_string()), files, uninstall_cmd: vec![], ..Default::default() };
        m.save(&dirs.state)?;
        Ok(m)
    }
}
