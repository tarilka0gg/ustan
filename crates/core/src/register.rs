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
];

/// Windows executables: opt-in, they usually belong to Wine/Proton launchers.
pub const MIME_EXE: &[&str] = &[
    "application/vnd.microsoft.portable-executable",
    "application/x-msdownload",
    "application/x-dosexec",
    "application/x-ms-dos-executable",
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

pub fn register(home: &Path, gui: &Path, with_exe: bool) -> Result<()> {
    let apps = home.join(".local/share/applications");
    std::fs::create_dir_all(&apps)?;
    let all: Vec<&str> = if with_exe { MIME_DEFAULT.iter().chain(MIME_EXE).copied().collect() } else { MIME_DEFAULT.to_vec() };
    let text = format!(
        "[Desktop Entry]\nType=Application\nName=Ustan\nComment=Install .deb, AppImage, Windows and Flatpak packages\nExec=\"{}\" %U\nIcon=system-software-install\nTerminal=false\nCategories=System;PackageManager;\nMimeType={};\nStartupNotify=true\n",
        gui.display(),
        all.join(";"),
    );
    std::fs::write(apps.join(DESKTOP), text)?;
    let _ = tool(home, "update-desktop-database", &[apps.to_str().unwrap()]);
    let mut args = vec!["default", DESKTOP];
    args.extend(MIME_DEFAULT);
    if with_exe {
        args.extend(MIME_EXE);
    }
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
    Ok(())
}
