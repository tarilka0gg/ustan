//! Make double-click in a file manager / browser open ustan.
use crate::{Error, Result};
use std::path::Path;
use std::process::Command;

const DESKTOP: &str = "io.github.tarilka0gg.Ustan.desktop";

/// Types we take over by default.
pub const MIME_DEFAULT: &[&str] = &[
    "application/vnd.debian.binary-package",
    "application/vnd.appimage",
    "application/x-iso9660-appimage",
    "application/vnd.flatpak",
    "application/vnd.flatpak.ref",
    "application/x-rpm",
    "application/vnd.snap",
];

/// Windows executables: opt-in, they usually belong to Wine/Proton launchers.
pub const MIME_EXE: &[&str] = &[
    "application/vnd.microsoft.portable-executable",
    "application/x-msdownload",
    "application/x-dosexec",
    "application/x-ms-dos-executable",
    "application/x-msi",
];

/// Archives: opt-in, they normally belong to the archive manager.
pub const MIME_ARCHIVE: &[&str] = &[
    "application/x-compressed-tar",
    "application/x-xz-compressed-tar",
    "application/x-zstd-compressed-tar",
    "application/x-bzip2-compressed-tar",
    "application/x-tar",
    "application/zip",
    "application/x-7z-compressed",
    "application/java-archive",
];

fn tool(home: &Path, prog: &str, args: &[&str]) -> Result<()> {
    let st = Command::new(prog)
        .args(args)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .status()
        .map_err(|e| Error::Format(format!("cannot run {prog}: {e}")))?;
    if st.success() { Ok(()) } else { Err(Error::Format(format!("{prog} failed: {st}"))) }
}

fn backup_path(home: &Path) -> std::path::PathBuf {
    home.join(".local/share/ustan/mime-backup.toml")
}

/// Who handled each type before us (`mime = "other.desktop"`), so `unregister` can give it back.
/// The first answer wins: registering twice must not record *ourselves* as the previous handler.
fn remember_previous(home: &Path, types: &[&str]) {
    let path = backup_path(home);
    let mut saved: std::collections::BTreeMap<String, String> =
        std::fs::read_to_string(&path).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default();
    for t in types {
        if saved.contains_key(*t) {
            continue;
        }
        let out = Command::new("xdg-mime").args(["query", "default", t]).env("HOME", home).env_remove("XDG_CONFIG_HOME").output();
        if let Ok(o) = out {
            let prev = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !prev.is_empty() && prev != DESKTOP {
                saved.insert(t.to_string(), prev);
            }
        }
    }
    if let Ok(t) = toml::to_string(&saved) {
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(path, t);
    }
}

pub fn register(home: &Path, gui: &Path, with_exe: bool, with_archives: bool) -> Result<()> {
    let apps = home.join(".local/share/applications");
    std::fs::create_dir_all(&apps)?;
    let mut all: Vec<&str> = MIME_DEFAULT.to_vec();
    if with_exe {
        all.extend(MIME_EXE);
    }
    if with_archives {
        all.extend(MIME_ARCHIVE);
    }
    let text = format!(
        "[Desktop Entry]\nType=Application\nName=Ustan\nComment=Install .deb, AppImage, Windows and Flatpak packages\nExec=\"{}\" %U\nIcon=system-software-install\nTerminal=false\nCategories=System;PackageManager;\nMimeType={};\nStartupNotify=true\n",
        gui.display(),
        all.join(";"),
    );
    std::fs::write(apps.join(DESKTOP), text)?;
    let _ = tool(home, "update-desktop-database", &[apps.to_str().unwrap()]);
    remember_previous(home, &all);
    let mut args = vec!["default", DESKTOP];
    args.extend(all.iter().copied());
    tool(home, "xdg-mime", &args)
}

pub fn unregister(home: &Path) -> Result<()> {
    let apps = home.join(".local/share/applications");
    let _ = std::fs::remove_file(apps.join(DESKTOP));
    let _ = tool(home, "update-desktop-database", &[apps.to_str().unwrap()]);
    let list = home.join(".config/mimeapps.list");
    if let Ok(s) = std::fs::read_to_string(&list) {
        let mut out = String::new();
        for l in s.lines() {
            match l.split_once('=') {
                Some((k, v)) if v.split(';').any(|t| t == DESKTOP) => {
                    let rest: Vec<&str> = v.split(';').filter(|t| !t.is_empty() && *t != DESKTOP).collect();
                    if !rest.is_empty() {
                        out.push_str(&format!("{k}={};\n", rest.join(";")));
                    }
                }
                _ => {
                    out.push_str(l);
                    out.push('\n');
                }
            }
        }
        std::fs::write(list, out)?;
    }
    // hand every type back to whatever handled it before we registered
    let backup = backup_path(home);
    if let Some(prev) = std::fs::read_to_string(&backup).ok().and_then(|s| toml::from_str::<std::collections::BTreeMap<String, String>>(&s).ok()) {
        for (mime, desktop) in prev {
            let _ = tool(home, "xdg-mime", &["default", &desktop, &mime]);
        }
        let _ = std::fs::remove_file(backup);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// register/unregister in a scratch HOME: the old handler must come back, new types vanish.
    #[test]
    fn unregister_restores_the_previous_defaults() {
        if crate::backend::tree::which("xdg-mime").is_none() {
            return; // nothing to test with
        }
        let h = std::env::temp_dir().join(format!("ustan-reg-{}", std::process::id()));
        std::fs::create_dir_all(h.join(".config")).unwrap();
        // the previous handler has to exist as a desktop file, or xdg-mime skips it
        std::fs::create_dir_all(h.join(".local/share/applications")).unwrap();
        std::fs::write(h.join(".local/share/applications/prev-handler.desktop"), "[Desktop Entry]\nType=Application\nName=Prev\nExec=true\nMimeType=application/vnd.flatpak;\n").unwrap();
        std::fs::write(h.join(".config/mimeapps.list"), "[Default Applications]\napplication/vnd.flatpak=prev-handler.desktop;\n").unwrap();
        let query = |m: &str| String::from_utf8_lossy(&Command::new("xdg-mime").args(["query", "default", m]).env("HOME", &h).output().unwrap().stdout).trim().to_string();

        register(&h, Path::new("/usr/bin/ustan-gui"), false, false).unwrap();
        assert_eq!(query("application/vnd.flatpak"), DESKTOP);
        assert_eq!(query("application/x-rpm"), DESKTOP);
        register(&h, Path::new("/usr/bin/ustan-gui"), false, false).unwrap(); // twice: still remembers the real previous one

        unregister(&h).unwrap();
        assert_eq!(query("application/vnd.flatpak"), "prev-handler.desktop");
        assert_ne!(query("application/x-rpm"), DESKTOP);
        let _ = std::fs::remove_dir_all(h);
    }
}
