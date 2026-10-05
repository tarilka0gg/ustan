//! Rewriting `.desktop` files so they point into the private install dir.
use std::path::{Path, PathBuf};

/// Map an absolute path from inside a package to its place under `root`.
fn under(root: &Path, p: &str) -> PathBuf {
    root.join(p.trim_start_matches('/'))
}

fn fix_exec(root: &Path, value: &str) -> String {
    let (cmd, rest) = value.split_once(char::is_whitespace).map_or((value, ""), |(a, b)| (a, b));
    let resolved = if cmd.starts_with('/') {
        Some(under(root, cmd))
    } else {
        ["usr/bin", "usr/local/bin", "bin", "usr/games"]
            .iter()
            .map(|d| root.join(d).join(cmd))
            .find(|p| p.exists())
    };
    let libs: Vec<String> = ["usr/lib", "usr/lib64", "usr/lib/x86_64-linux-gnu", "lib/x86_64-linux-gnu"]
        .iter()
        .map(|d| root.join(d))
        .filter(|p| p.is_dir())
        .map(|p| p.display().to_string())
        .collect();
    match resolved {
        Some(p) if libs.is_empty() => format!("{} {}", p.display(), rest).trim_end().to_string(),
        Some(p) => format!("env LD_LIBRARY_PATH={} {} {}", libs.join(":"), p.display(), rest)
            .trim_end()
            .to_string(),
        None => value.to_string(),
    }
}

fn icon_score(p: &Path) -> u32 {
    if p.extension().is_some_and(|e| e == "svg") {
        return u32::MAX;
    }
    p.components()
        .filter_map(|c| c.as_os_str().to_str())
        .filter_map(|s| s.split('x').next()?.parse::<u32>().ok())
        .max()
        .unwrap_or(1)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() { walk(&p, out) } else { out.push(p) }
        }
    }
}

/// Resolve an `Icon=` value to a file inside `root`, preferring SVG then the largest raster.
pub fn find_icon(root: &Path, icon: &str) -> Option<PathBuf> {
    if icon.starts_with('/') {
        let p = under(root, icon);
        return p.is_file().then_some(p);
    }
    let mut all = Vec::new();
    for d in ["usr/share/icons", "usr/share/pixmaps", "usr/local/share/icons", "opt"] {
        walk(&root.join(d), &mut all);
    }
    all.into_iter()
        .filter(|p| {
            p.file_stem().is_some_and(|s| s == icon)
                && p.extension().is_some_and(|e| matches!(e.to_str(), Some("png" | "svg" | "xpm")))
        })
        .max_by_key(|p| icon_score(p))
}

/// Rewrite Exec/TryExec/Icon/Path in a desktop entry. Returns the new text.
pub fn rewrite(text: &str, root: &Path) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let new = match line.split_once('=') {
            Some(("Exec", v)) => format!("Exec={}", fix_exec(root, v)),
            Some(("TryExec", _)) | Some(("DBusActivatable", _)) => continue,
            Some(("Path", _)) => continue,
            Some(("Icon", v)) => match find_icon(root, v) {
                Some(p) => format!("Icon={}", p.display()),
                None => line.to_string(),
            },
            _ => line.to_string(),
        };
        out.push_str(&new);
        out.push('\n');
    }
    out
}

pub fn name(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.strip_prefix("Name=")).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_exec_and_icon() {
        let r = std::env::temp_dir().join(format!("ustan-d-{}", std::process::id()));
        std::fs::create_dir_all(r.join("usr/bin")).unwrap();
        std::fs::create_dir_all(r.join("usr/share/icons/hicolor/48x48/apps")).unwrap();
        std::fs::create_dir_all(r.join("usr/share/icons/hicolor/256x256/apps")).unwrap();
        std::fs::write(r.join("usr/bin/foo"), "").unwrap();
        std::fs::write(r.join("usr/share/icons/hicolor/48x48/apps/foo.png"), "").unwrap();
        std::fs::write(r.join("usr/share/icons/hicolor/256x256/apps/foo.png"), "").unwrap();
        let t = rewrite("[Desktop Entry]\nName=Foo\nExec=foo %U\nIcon=foo\nTryExec=foo\n", &r);
        assert!(t.contains(&format!("Exec={}/usr/bin/foo %U", r.display())), "{t}");
        assert!(t.contains("256x256/apps/foo.png"), "{t}");
        assert!(!t.contains("TryExec"));
        let _ = std::fs::remove_dir_all(r);
    }
}
