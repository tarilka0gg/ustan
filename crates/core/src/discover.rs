//! Find AppImages that live outside ustan's own directory (downloaded to ~/Games, ~/Downloads, ...).
//! They are not managed, but we can still check them for updates and update them in place.
use crate::backend::appimage;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Found {
    pub path: PathBuf,
    pub name: String,
    pub version: Option<String>,
    pub update_info: Option<String>,
}

/// Directory names that are never worth descending into (caches, toolchains, wine/steam prefixes...).
const SKIP: &[&str] = &[
    "node_modules", "target", "Steam", "steamapps", "compatdata", "pfx", "drive_c", "Trash", "snap", "__pycache__", "venv",
];

fn skip_dir(name: &str) -> bool {
    // Hidden dirs are skipped, except ~/.local (AppImages often live in ~/.local/bin or ~/.local/share).
    (name.starts_with('.') && name != ".local") || SKIP.contains(&name)
}

fn walk(dir: &Path, depth: u32, exclude: &[PathBuf], out: &mut BTreeSet<PathBuf>) {
    if depth == 0 || exclude.iter().any(|e| dir.starts_with(e)) {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if ft.is_dir() {
            if !skip_dir(&name) {
                walk(&p, depth - 1, exclude, out);
            }
        } else if ft.is_file() && name.to_lowercase().ends_with(".appimage") && appimage::is_appimage(&p) {
            out.insert(p);
        }
    }
}

/// First absolute path in an `Exec=` line that exists (skips `env VAR=x` prefixes and quotes).
fn exec_target(line: &str) -> Option<PathBuf> {
    let v = line.strip_prefix("Exec=")?;
    v.split_whitespace()
        .map(|t| t.trim_matches('"'))
        .find(|t| t.starts_with('/'))
        .map(PathBuf::from)
        .filter(|p| p.is_file())
}

fn desktop_entries(home: &Path, out: &mut BTreeSet<PathBuf>) {
    for d in [home.join(".local/share/applications"), "/usr/share/applications".into(), "/usr/local/share/applications".into()] {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        for e in rd.flatten() {
            let Ok(text) = std::fs::read_to_string(e.path()) else { continue };
            for l in text.lines() {
                if let Some(p) = exec_target(l) {
                    // Launchers may point at AppImages with any file name.
                    if appimage::is_appimage(&p) {
                        out.insert(p);
                    }
                }
            }
        }
    }
}

/// All unmanaged AppImages reachable from `home`'s desktop entries and a bounded filesystem walk.
/// `exclude` holds directories to ignore (ustan's own opt/state dirs).
pub fn find_appimages(home: &Path, exclude: &[PathBuf]) -> Vec<Found> {
    let mut paths = BTreeSet::new();
    desktop_entries(home, &mut paths);
    walk(home, 8, exclude, &mut paths);
    walk(Path::new("/opt"), 4, exclude, &mut paths);

    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for p in paths {
        let real = std::fs::canonicalize(&p).unwrap_or(p.clone());
        if exclude.iter().any(|e| real.starts_with(e)) || !seen.insert(real) {
            continue;
        }
        if let Ok(pr) = appimage::probe(&p) {
            out.push(Found { path: p, name: pr.name, version: pr.version, update_info: pr.update_info });
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_lines() {
        assert_eq!(exec_target("Exec=/bin/sh -c x"), Some(PathBuf::from("/bin/sh")));
        assert_eq!(exec_target("Exec=env \"A=1\" \"/bin/sh\" %U"), Some(PathBuf::from("/bin/sh")));
        assert_eq!(exec_target("Exec=firefox"), None);
        assert_eq!(exec_target("Name=x"), None);
    }

    #[test]
    fn skips_hidden_and_noise() {
        assert!(skip_dir(".cache") && skip_dir("node_modules") && skip_dir("Steam"));
        assert!(!skip_dir(".local") && !skip_dir("Games"));
    }
}
