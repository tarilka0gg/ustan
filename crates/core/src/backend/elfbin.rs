//! A bare Linux executable (a GitHub release asset like `tool-linux-amd64`): copy it into its own
//! directory and make it launchable. A program linking a GUI toolkit gets a menu entry, the rest
//! is a command-line tool and lands in `~/.local/bin`.
use super::archive::{stem, version_of};
use super::tree::{clean_name, install_tree, is_gui_exe, make_launcher, Spec};
use super::{slug, Backend, Info, Opts};
use crate::{dirs::Dirs, manifest::Manifest, Error, Result};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub struct ElfBin;

fn is_elf(path: &Path) -> bool {
    let mut b = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut b)).is_ok() && &b == b"\x7fELF"
}

fn names(path: &Path) -> (String, Option<String>) {
    let st = stem(path);
    let file = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    // `tool` has no extension; `tool-1.2-linux-amd64.bin` loses `.bin` through clean_name
    let base = if file.ends_with(".bin") || file.ends_with(".elf") || file.ends_with(".x86_64") { st.rsplit_once('.').map_or(st.clone(), |(a, _)| a.to_string()) } else { st };
    (clean_name(&base), version_of(&base))
}

impl Backend for ElfBin {
    fn kind(&self) -> &'static str {
        "elf"
    }

    fn detect(&self, path: &Path) -> bool {
        path.is_file() && is_elf(path) && !super::appimage::is_appimage(path)
    }

    fn inspect(&self, path: &Path) -> Result<Info> {
        let (name, version) = names(path);
        Ok(Info { id: slug(&name), name, version, kind: "elf", icon: None, warning: None })
    }

    fn install(&self, path: &Path, dirs: &Dirs, opts: &Opts) -> Result<Manifest> {
        if !self.detect(path) {
            return Err(Error::Format("не виконуваний файл ELF".into()));
        }
        let (name, version) = names(path);
        let file = path.file_name().unwrap().to_owned();
        let gui = is_gui_exe(path);
        let spec = Spec { kind: "elf", id: slug(&name), name: name.clone(), version, source: path, overlay: false };
        install_tree(
            spec,
            dirs,
            opts,
            |root| {
                let dst = root.join(&file);
                std::fs::copy(path, &dst)?;
                std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755))?;
                Ok(())
            },
            |root, dirs, id, files| make_launcher(root, dirs, id, &name, &root.join(&file), gui, Some(&name), files),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_lose_platform_words_and_versions() {
        assert_eq!(names(Path::new("/d/tool-linux-amd64")), ("tool".into(), None));
        assert_eq!(names(Path::new("/d/ripgrep-14.1.0-x86_64-unknown-linux-musl")), ("ripgrep".into(), Some("14.1.0".into())));
        assert_eq!(names(Path::new("/d/app_v2.5.bin")), ("app".into(), Some("2.5".into())));
        assert_eq!(names(Path::new("/d/plain")), ("plain".into(), None));
    }
}
