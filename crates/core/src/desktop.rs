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
    find_icon_in(root, icon, &["usr/share/icons", "usr/share/pixmaps", "usr/local/share/icons", "opt"])
}

/// Like [`find_icon`], but searches the whole tree (for archives with an arbitrary layout).
pub fn find_icon_deep(root: &Path, icon: &str) -> Option<PathBuf> {
    find_icon_in(root, icon, &[""])
}

fn find_icon_in(root: &Path, icon: &str, dirs: &[&str]) -> Option<PathBuf> {
    if icon.starts_with('/') {
        let p = under(root, icon);
        return p.is_file().then_some(p);
    }
    let mut all = Vec::new();
    for d in dirs {
        walk(&root.join(d), &mut all);
    }
    let is_img = |p: &PathBuf| p.extension().is_some_and(|e| matches!(e.to_str(), Some("png" | "svg" | "xpm")));
    let exact = all.iter().filter(|p| is_img(p) && p.file_stem().is_some_and(|s| s == icon)).max_by_key(|p| icon_score(p));
    if let Some(p) = exact {
        return Some(p.clone());
    }
    // Packages often ship the icon under another name (e.g. opt/x/product_logo_256.png).
    let want = icon.to_lowercase();
    all.iter()
        .filter(|p| {
            is_img(p) && {
                let s = p.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
                // an icon-ish name, or any image that sits in an `icons`/`pixmaps` directory
                let in_icon_dir = p.parent().is_some_and(|d| d.components().any(|c| matches!(c.as_os_str().to_str(), Some("icons" | "icon" | "pixmaps"))));
                (!want.is_empty() && s.contains(&want)) || s.contains("logo") || s.contains("icon") || in_icon_dir
            }
        })
        .max_by_key(|p| icon_score(p).max(trailing_number(p)))
        .cloned()
}

fn trailing_number(p: &Path) -> u32 {
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    stem.rsplit(|c: char| !c.is_ascii_digit()).next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

/// Rewrite Exec/TryExec/Icon/Path in a desktop entry. Returns the new text.
pub fn rewrite(text: &str, root: &Path, launcher: Option<&Path>) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let new = match line.split_once('=') {
            Some(("Exec", v)) => match launcher {
                // run the (already rooted) command through the overlay launcher
                Some(l) => format!("Exec=\"{}\" {}", l.display(), fix_exec(root, v)),
                None => format!("Exec={}", fix_exec(root, v)),
            },
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
    fn icon_in_an_icons_directory_is_found() {
        let r = std::env::temp_dir().join(format!("ustan-j-{}", std::process::id()));
        std::fs::create_dir_all(r.join("app/browser/chrome/icons/default")).unwrap();
        std::fs::create_dir_all(r.join("app/browser/res")).unwrap();
        for n in ["default16.png", "default128.png", "default48.png"] {
            std::fs::write(r.join("app/browser/chrome/icons/default").join(n), "").unwrap();
        }
        std::fs::write(r.join("app/browser/res/splash.png"), "").unwrap();
        let p = find_icon_deep(&r, "Firefox").unwrap();
        assert!(p.ends_with("default128.png"), "{p:?}");
        let _ = std::fs::remove_dir_all(r);
    }

    #[test]
    fn icon_falls_back_to_logo_files() {
        let r = std::env::temp_dir().join(format!("ustan-i-{}", std::process::id()));
        std::fs::create_dir_all(r.join("opt/app")).unwrap();
        for n in ["product_logo_32.png", "product_logo_256.png", "other.png"] {
            std::fs::write(r.join("opt/app").join(n), "").unwrap();
        }
        let p = find_icon(&r, "my-app").unwrap();
        assert!(p.ends_with("product_logo_256.png"), "{p:?}");
        let _ = std::fs::remove_dir_all(r);
    }

    #[test]
    fn rewrites_exec_and_icon() {
        let r = std::env::temp_dir().join(format!("ustan-d-{}", std::process::id()));
        std::fs::create_dir_all(r.join("usr/bin")).unwrap();
        std::fs::create_dir_all(r.join("usr/share/icons/hicolor/48x48/apps")).unwrap();
        std::fs::create_dir_all(r.join("usr/share/icons/hicolor/256x256/apps")).unwrap();
        std::fs::write(r.join("usr/bin/foo"), "").unwrap();
        std::fs::write(r.join("usr/share/icons/hicolor/48x48/apps/foo.png"), "").unwrap();
        std::fs::write(r.join("usr/share/icons/hicolor/256x256/apps/foo.png"), "").unwrap();
        let t = rewrite("[Desktop Entry]\nName=Foo\nExec=foo %U\nIcon=foo\nTryExec=foo\n", &r, None);
        let w = rewrite("[Desktop Entry]\nExec=foo %U\n", &r, Some(Path::new("/x/launch")));
        assert!(w.contains("Exec=\"/x/launch\" ") && w.contains("/usr/bin/foo %U"), "{w}");
        assert!(t.contains(&format!("Exec={}/usr/bin/foo %U", r.display())), "{t}");
        assert!(t.contains("256x256/apps/foo.png"), "{t}");
        assert!(!t.contains("TryExec"));
        let _ = std::fs::remove_dir_all(r);
    }
}
