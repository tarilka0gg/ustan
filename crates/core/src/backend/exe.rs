//! Windows executables run through Wine, each app in its own prefix.
//! A local file is registered in place (games keep their data next to the .exe);
//! a downloaded one (inside our cache) is copied into the app dir first.
use super::{slug, Backend, Info, Opts};
use crate::{dirs::Dirs, manifest::Manifest, pe, Error, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Exe;

fn read_exe(path: &Path) -> Result<Vec<u8>> {
    let data = std::fs::read(path)?;
    if data.len() < 0x40 || &data[..2] != b"MZ" {
        return Err(Error::Format("not a Windows executable".into()));
    }
    Ok(data)
}

fn stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "app".into())
}

impl Backend for Exe {
    fn kind(&self) -> &'static str {
        "exe"
    }

    fn detect(&self, path: &Path) -> bool {
        path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))
    }

    fn inspect(&self, path: &Path) -> Result<Info> {
        let data = read_exe(path)?;
        let n = stem(path);
        let icon = pe::icon_png(&data).ok().flatten().map(|bytes| super::Icon { ext: "png", bytes });
        Ok(Info { id: slug(&n), name: n, version: None, kind: "exe", icon })
    }

    fn install(&self, path: &Path, dirs: &Dirs, opts: &Opts) -> Result<Manifest> {
        if opts.installer {
            return install_installer(path, dirs);
        }
        let data = read_exe(path)?;
        let name = stem(path);
        let id = slug(&name);
        let root = dirs.opt.join(&id);
        if root.exists() {
            return Err(Error::Format(format!("`{id}` is already installed")));
        }
        std::fs::create_dir_all(&root)?;
        let mut files = vec![root.clone()];

        let res = (|| -> Result<()> {
            let abs = std::fs::canonicalize(path)?;
            let exe = if abs.starts_with(dirs.state.join("cache")) {
                let dst = root.join(abs.file_name().unwrap());
                std::fs::copy(&abs, &dst)?;
                dst
            } else {
                abs
            };

            let icon = match pe::icon_png(&data)? {
                Some(png) => {
                    let p = root.join("icon.png");
                    std::fs::write(&p, png)?;
                    Some(p)
                }
                None => None,
            };

            let wd = exe.parent().unwrap_or(Path::new("/"));
            let mut t = format!(
                "[Desktop Entry]\nType=Application\nName={name}\nExec=env \"WINEPREFIX={}\" wine \"{}\"\nPath={}\nCategories=Wine;\nStartupWMClass={}\n",
                root.join("prefix").display(),
                exe.display(),
                wd.display(),
                exe.file_name().unwrap().to_string_lossy().to_lowercase(),
            );
            if let Some(i) = icon {
                t.push_str(&format!("Icon={}\n", i.display()));
            }
            std::fs::create_dir_all(&dirs.apps)?;
            let dst = dirs.apps.join(format!("ustan-{id}.desktop"));
            std::fs::write(&dst, t)?;
            files.push(dst);
            Ok(())
        })();
        if let Err(e) = res {
            for f in files.iter().rev() {
                let _ = if f.is_dir() { std::fs::remove_dir_all(f) } else { std::fs::remove_file(f) };
            }
            return Err(e);
        }
        let m = Manifest { id, name, version: None, kind: "exe".into(), source: Some(path.display().to_string()), files, uninstall_cmd: vec![], ..Default::default() };
        m.save(&dirs.state)?;
        Ok(m)
    }
}

/// Resolve a Windows path inside a wine prefix, matching each segment case-insensitively.
pub fn win_to_unix(prefix: &Path, win: &str) -> Option<PathBuf> {
    let (drive, rest) = win.split_once(':')?;
    let mut cur = match drive.to_ascii_lowercase().as_str() {
        "c" => prefix.join("drive_c"),
        "z" => PathBuf::from("/"),
        _ => return None,
    };
    for seg in rest.split(['\\', '/']).filter(|s| !s.is_empty()) {
        let direct = cur.join(seg);
        cur = if direct.exists() {
            direct
        } else {
            std::fs::read_dir(&cur).ok()?.flatten().map(|e| e.path()).find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(seg)))?
        };
    }
    Some(cur)
}

fn find_lnks(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                find_lnks(&p, out);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")) {
                out.push(p);
            }
        }
    }
}

fn start_menus(prefix: &Path) -> Vec<PathBuf> {
    let c = prefix.join("drive_c");
    let mut v = vec![c.join("ProgramData/Microsoft/Windows/Start Menu")];
    if let Ok(rd) = std::fs::read_dir(c.join("users")) {
        for u in rd.flatten() {
            v.push(u.path().join("AppData/Roaming/Microsoft/Windows/Start Menu"));
        }
    }
    v
}

/// Run a Windows installer in a fresh prefix, then turn the Start Menu shortcuts it made into launchers.
fn install_installer(path: &Path, dirs: &Dirs) -> Result<Manifest> {
    read_exe(path)?;
    let name = stem(path);
    let id = slug(&name);
    let root = dirs.opt.join(&id);
    if root.exists() {
        return Err(Error::Format(format!("`{id}` is already installed")));
    }
    let prefix = root.join("prefix");
    std::fs::create_dir_all(&prefix)?;
    let mut files = vec![root.clone()];
    let cleanup = |files: &[PathBuf]| {
        for f in files.iter().rev() {
            let _ = if f.is_dir() { std::fs::remove_dir_all(f) } else { std::fs::remove_file(f) };
        }
    };

    // winemenubuilder off: we make the launchers ourselves, wine must not litter ~/.local/share/applications.
    let st = Command::new("wine")
        .arg(std::fs::canonicalize(path)?)
        .env("WINEPREFIX", &prefix)
        .env("WINEDLLOVERRIDES", "winemenubuilder.exe=d")
        .env("WINEDEBUG", "-all")
        .status();
    let st = match st {
        Ok(s) => s,
        Err(e) => {
            cleanup(&files);
            return Err(Error::Format(format!("cannot run wine: {e}")));
        }
    };
    let _ = Command::new("wineserver").arg("-w").env("WINEPREFIX", &prefix).status();

    let res = (|| -> Result<usize> {
        let mut lnks = Vec::new();
        for m in start_menus(&prefix) {
            find_lnks(&m, &mut lnks);
        }
        std::fs::create_dir_all(&dirs.apps)?;
        std::fs::create_dir_all(root.join("icons"))?;
        let mut seen = std::collections::HashSet::new();
        for l in lnks {
            let label = l.file_stem().unwrap().to_string_lossy().into_owned();
            let Ok(sc) = crate::lnk::parse(&std::fs::read(&l)?) else { continue };
            let Some(exe) = win_to_unix(&prefix, &sc.target) else { continue };
            let low = exe.file_name().unwrap().to_string_lossy().to_lowercase();
            if !low.ends_with(".exe") || low.starts_with("unins") || label.to_lowercase().contains("uninstall") || !seen.insert(exe.clone()) {
                continue;
            }
            let icon = std::fs::read(&exe).ok().and_then(|d| pe::icon_png(&d).ok().flatten());
            let lid = slug(&label);
            let mut t = format!(
                "[Desktop Entry]\nType=Application\nName={label}\nExec=env \"WINEPREFIX={}\" wine \"{}\"{}\nPath={}\nCategories=Wine;\nStartupWMClass={low}\n",
                prefix.display(),
                exe.display(),
                if sc.args.is_empty() { String::new() } else { format!(" {}", sc.args) },
                exe.parent().unwrap().display(),
            );
            if let Some(png) = icon {
                let ip = root.join("icons").join(format!("{lid}.png"));
                std::fs::write(&ip, png)?;
                t.push_str(&format!("Icon={}\n", ip.display()));
            }
            let dst = dirs.apps.join(format!("ustan-{id}-{lid}.desktop"));
            std::fs::write(&dst, t)?;
            files.push(dst);
        }
        Ok(files.len() - 1)
    })();
    match res {
        Ok(n) if n > 0 => {}
        Ok(_) => {
            cleanup(&files);
            return Err(Error::Format(format!("installer finished ({st}) but created no Start Menu shortcuts; try `ustan install` on the installed .exe directly")));
        }
        Err(e) => {
            cleanup(&files);
            return Err(e);
        }
    }
    let m = Manifest { id, name, version: None, kind: "exe-installer".into(), source: Some(path.display().to_string()), files, uninstall_cmd: vec![], ..Default::default() };
    m.save(&dirs.state)?;
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_windows_paths_case_insensitively() {
        let r = std::env::temp_dir().join(format!("ustan-w-{}", std::process::id()));
        std::fs::create_dir_all(r.join("drive_c/Program Files/App")).unwrap();
        std::fs::write(r.join("drive_c/Program Files/App/a.exe"), "").unwrap();
        let p = win_to_unix(&r, "C:\\program files\\APP\\a.exe").unwrap();
        assert!(p.ends_with("Program Files/App/a.exe"));
        assert!(win_to_unix(&r, "D:\\x").is_none());
        let _ = std::fs::remove_dir_all(r);
    }
}
