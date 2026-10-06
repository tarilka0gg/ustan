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

/// Windows executables and installers.
pub const MIME_EXE: &[&str] = &[
    "application/vnd.microsoft.portable-executable",
    "application/x-msdownload",
    "application/x-dosexec",
    "application/x-ms-dos-executable",
    "application/x-msi",
];

/// Archives and jars. ustan hands them back to the previous handler when they hold nothing installable.
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

/// Every type ustan can open.
pub fn all_types() -> Vec<&'static str> {
    MIME_DEFAULT.iter().chain(MIME_EXE).chain(MIME_ARCHIVE).copied().collect()
}

/// The `.desktop` file of whatever opened `mime` before ustan took it over (from the backup made by
/// `register`), so a zip full of photos can still go to the archive manager.
pub fn previous_handler(home: &Path, mime: &str) -> Option<std::path::PathBuf> {
    let prev: std::collections::BTreeMap<String, String> = toml::from_str(&std::fs::read_to_string(backup_path(home)).ok()?).ok()?;
    let id = prev.get(mime)?;
    [
        home.join(".local/share/applications"),
        "/usr/local/share/applications".into(),
        "/usr/share/applications".into(),
        home.join(".local/share/flatpak/exports/share/applications"),
        "/var/lib/flatpak/exports/share/applications".into(),
    ]
    .into_iter()
    .map(|d| d.join(id))
    .find(|p| p.is_file())
}

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

/// Some tools (PortProton) write one line for several types: `type1;type2;=app.desktop`. `xdg-mime`
/// reads those as a default for every listed type, so they would keep winning over our own lines.
/// Remove our types from such lines (keeping the others) and return who had them: `type -> desktop`.
/// The original file is saved once as `mimeapps.list.pre-ustan`.
fn split_multi_type_lines(home: &Path, ours: &[&str]) -> std::collections::BTreeMap<String, String> {
    let mut prev = std::collections::BTreeMap::new();
    let path = home.join(".config/mimeapps.list");
    let Ok(text) = std::fs::read_to_string(&path) else { return prev };
    let mut out = String::new();
    let mut changed = false;
    for line in text.lines() {
        match line.split_once('=') {
            Some((k, v)) if k.contains(';') && k.split(';').any(|t| ours.contains(&t)) => {
                let desktop = v.split(';').next().unwrap_or("").trim().to_string();
                let (hit, rest): (Vec<&str>, Vec<&str>) = k.split(';').filter(|t| !t.is_empty()).partition(|t| ours.contains(t));
                for t in hit {
                    if !desktop.is_empty() {
                        prev.insert(t.to_string(), desktop.clone());
                    }
                }
                changed = true;
                if !rest.is_empty() {
                    out.push_str(&format!("{};={v}\n", rest.join(";")));
                }
            }
            _ => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    if changed {
        let bak = path.with_file_name("mimeapps.list.pre-ustan");
        if !bak.exists() {
            let _ = std::fs::copy(&path, bak);
        }
        let _ = std::fs::write(&path, out);
    }
    prev
}

/// Who handled each type before us (`mime = "other.desktop"`), so `unregister` can give it back.
/// The first answer wins: registering twice must not record *ourselves* as the previous handler.
fn remember_previous(home: &Path, types: &[&str], known: std::collections::BTreeMap<String, String>) {
    let path = backup_path(home);
    let mut saved: std::collections::BTreeMap<String, String> =
        std::fs::read_to_string(&path).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default();
    for (t, d) in known {
        saved.entry(t).or_insert(d);
    }
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

/// A path for `Exec=`: quoted only when it has characters the spec makes special. `xdg-mime` takes
/// the quotes as part of the file name and then cannot find the program, so it ignores the handler.
fn exec_path(p: &Path) -> String {
    let s = p.display().to_string();
    if s.chars().any(|c| c.is_whitespace() || "\"'\\<>~|&;$*?#()`".contains(c)) { format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")) } else { s }
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
        "[Desktop Entry]\nType=Application\nName=Ustan\nComment=Install .deb, .rpm, .snap, AppImage, Flatpak, Windows and archive packages\nExec={} %U\nIcon=system-software-install\nTerminal=false\nCategories=System;PackageManager;\nMimeType={};\nStartupNotify=true\n",
        exec_path(gui),
        all.join(";"),
    );
    std::fs::write(apps.join(DESKTOP), text)?;
    let _ = tool(home, "update-desktop-database", &[apps.to_str().unwrap()]);
    let from_lines = split_multi_type_lines(home, &all);
    remember_previous(home, &all, from_lines);
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

    #[test]
    fn exec_paths_are_quoted_only_when_needed() {
        assert_eq!(exec_path(Path::new("/usr/bin/ustan-gui")), "/usr/bin/ustan-gui");
        assert_eq!(exec_path(Path::new("/opt/My Apps/ustan-gui")), "\"/opt/My Apps/ustan-gui\"");
    }

    #[test]
    fn multi_type_lines_lose_our_types_but_keep_the_rest() {
        let h = std::env::temp_dir().join(format!("ustan-multi-{}", std::process::id()));
        std::fs::create_dir_all(h.join(".config")).unwrap();
        std::fs::write(
            h.join(".config/mimeapps.list"),
            "[Default Applications]\nimage/png=imv.desktop\napplication/x-ms-dos-executable;application/x-wine-extension-msp;application/x-msi;text/win-bat;=PortProton.desktop\napplication/x-msdos-program;application/x-wine-extension-msp;=OnlyOthers.desktop\n",
        )
        .unwrap();
        let prev = split_multi_type_lines(&h, &["application/x-ms-dos-executable", "application/x-msi"]);
        assert_eq!(prev.get("application/x-msi").map(String::as_str), Some("PortProton.desktop"));
        let t = std::fs::read_to_string(h.join(".config/mimeapps.list")).unwrap();
        assert!(t.contains("application/x-wine-extension-msp;text/win-bat;=PortProton.desktop"), "{t}");
        assert!(!t.contains("x-ms-dos-executable") && !t.contains("application/x-msi"), "{t}");
        assert!(t.contains("image/png=imv.desktop") && t.contains("OnlyOthers.desktop"), "untouched lines stay: {t}");
        assert!(h.join(".config/mimeapps.list.pre-ustan").exists(), "the original is kept");
        let _ = std::fs::remove_dir_all(h);
    }

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
        // the explicit entry is gone (xdg-mime may still answer with a packaged .desktop that lists the type)
        let list = std::fs::read_to_string(h.join(".config/mimeapps.list")).unwrap();
        assert!(!list.contains(&format!("application/x-rpm={DESKTOP}")), "{list}");
        let _ = std::fs::remove_dir_all(h);
    }
}
