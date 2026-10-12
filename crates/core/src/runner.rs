//! Finding a Wine to run Windows programs: a system `wine`, or the one bundled with PortProton,
//! Steam's Proton or a compatibility tool (GE-Proton, ...). Launchers do not hard-code the choice:
//! they call a small wrapper script (`<state>/runner/wine`) that ustan rewrites when the runner changes.
use crate::{dirs::Dirs, Error, Result};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Runner {
    /// Short name shown to the user and accepted by `ustan runner use`.
    pub name: String,
    /// `None` for the system `wine` found in PATH.
    pub root: Option<PathBuf>,
    pub wine: PathBuf,
    pub wineserver: PathBuf,
    rank: u8,
}

fn from_root(name: String, root: &Path, rank: u8) -> Option<Runner> {
    let bin = root.join("bin");
    let wine = ["wine64", "wine"].iter().map(|n| bin.join(n)).find(|p| p.is_file())?;
    let wineserver = bin.join("wineserver");
    wineserver.is_file().then(|| Runner { name, root: Some(root.to_path_buf()), wine, wineserver, rank })
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default()
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// A tool dir is either `<dir>/bin/wine` or `<dir>/files/bin/wine` (Proton layout).
fn tool_root(dir: &Path) -> Option<PathBuf> {
    [dir.to_path_buf(), dir.join("files")].into_iter().find(|r| r.join("bin").is_dir())
}

fn runners_dir(home: &Path) -> PathBuf {
    home.join(".local/share/ustan/runners")
}

const CACHYOS_API: &str = "https://api.github.com/repos/CachyOS/proton-cachyos/releases/latest";

fn has_avx2() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("bmi2") && std::arch::is_x86_feature_detected!("fma")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// Download the latest proton-cachyos (CachyOS/proton-cachyos on GitHub; the x86_64_v3 build when the
/// CPU has AVX2), check its sha512 and unpack it into ustan's runners dir. Returns its runner.
pub fn get_cachyos(dirs: &Dirs) -> Result<Runner> {
    use sha2::{Digest, Sha512};
    let home = home();
    let rel: serde_json::Value = ureq::get(CACHYOS_API)
        .set("User-Agent", "ustan")
        .call()
        .map_err(|e| Error::Format(format!("GitHub: {e}")))?
        .into_json()
        .map_err(|e| Error::Format(format!("GitHub: {e}")))?;
    let assets = rel["assets"].as_array().ok_or_else(|| Error::Format("GitHub: у релізі немає файлів".into()))?;
    let suffix = if has_avx2() { "-x86_64_v3.tar.xz" } else { "-x86_64.tar.xz" };
    let url_of = |name_end: &str| assets.iter().find(|a| a["name"].as_str().is_some_and(|n| n.ends_with(name_end))).and_then(|a| a["browser_download_url"].as_str());
    let url = url_of(suffix).ok_or_else(|| Error::Format(format!("у релізі немає *{suffix}")))?;
    let name = url.rsplit('/').next().unwrap_or("proton-cachyos").trim_end_matches(".tar.xz").to_string();
    let dest = runners_dir(&home).join(&name);
    if let Some(r) = tool_root(&dest).and_then(|r| from_root(format!("ustan/{name}"), &r, 3)) {
        return Ok(r); // already here
    }
    let want = url_of(&suffix.replace(".tar.xz", ".sha512sum"))
        .and_then(|u| ureq::get(u).set("User-Agent", "ustan").call().ok())
        .and_then(|r| r.into_string().ok())
        .and_then(|t| t.split_whitespace().next().map(str::to_lowercase));
    let got = crate::fetch::download_checked(url, &dirs.state.join("cache"), &crate::fetch::Expect::None, &name)?;
    if let Some(want) = want {
        let mut h = Sha512::new();
        std::io::copy(&mut std::fs::File::open(&got.path)?, &mut h)?;
        let have: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if have != want {
            let _ = std::fs::remove_file(&got.path);
            return Err(Error::Format("sha512 не збігається: файл пошкоджений".into()));
        }
    }
    crate::progress::status("Розпаковую Proton…");
    let tmp = runners_dir(&home).join(format!(".{name}.part"));
    let _ = std::fs::remove_dir_all(&tmp);
    crate::backend::archive::extract_to(&got.path, &tmp)?;
    // the tarball holds one directory; take it, or the tmp dir itself
    let inner = subdirs(&tmp);
    let src = if inner.len() == 1 && tool_root(&inner[0]).is_some() { inner[0].clone() } else { tmp.clone() };
    std::fs::rename(&src, &dest)?;
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_file(&got.path);
    tool_root(&dest).and_then(|r| from_root(format!("ustan/{name}"), &r, 3)).ok_or_else(|| Error::Format("у архіві немає bin/wine".into()))
}

/// Every Wine we can find, best first.
pub fn discover(home: &Path) -> Vec<Runner> {
    let mut out = Vec::new();
    if let Some(w) = crate::backend::tree::which("wine") {
        let ws = crate::backend::tree::which("wineserver").unwrap_or_else(|| PathBuf::from("wineserver"));
        out.push(Runner { name: "system".into(), root: None, wine: w, wineserver: ws, rank: 0 });
    }
    // PortProton: plain Wine builds first, then its Proton flavours
    for d in subdirs(&home.join(".var/app/ru.linux_gaming.PortProton/data/dist")) {
        let n = name_of(&d);
        let rank = if n.starts_with("WINE") { 1 } else if n.starts_with("PROTON_LG") { 2 } else { 3 };
        if let Some(r) = tool_root(&d).and_then(|r| from_root(format!("portproton/{n}"), &r, rank)) {
            out.push(r);
        }
    }
    // runners ustan downloaded itself (`ustan runner get cachyos`)
    for d in subdirs(&runners_dir(home)) {
        if let Some(r) = tool_root(&d).and_then(|r| from_root(format!("ustan/{}", name_of(&d)), &r, 3)) {
            out.push(r);
        }
    }
    for d in subdirs(&home.join(".local/share/Steam/compatibilitytools.d")) {
        if let Some(r) = tool_root(&d).and_then(|r| from_root(format!("steam-tool/{}", name_of(&d)), &r, 4)) {
            out.push(r);
        }
    }
    for d in subdirs(&home.join(".local/share/Steam/steamapps/common")) {
        let n = name_of(&d);
        if n.starts_with("Proton") && !n.contains("Runtime") && !n.contains("Hotfix") {
            if let Some(r) = tool_root(&d).and_then(|r| from_root(format!("proton/{n}"), &r, 5)) {
                out.push(r);
            }
        }
    }
    // best rank first; within a rank the newest-looking name (they sort naturally by version)
    out.sort_by(|a, b| a.rank.cmp(&b.rank).then_with(|| b.name.cmp(&a.name)));
    out
}

/// The runner to use: the configured one, else the best discovered.
pub fn pick(dirs: &Dirs, home: &Path) -> Option<Runner> {
    let all = discover(home);
    if let Some(want) = crate::config::load(dirs).wine {
        if let Some(r) = all.iter().find(|r| r.name == want) {
            return Some(r.clone());
        }
        if let Some(r) = from_root(want.clone(), Path::new(&want).parent().and_then(Path::parent)?, 0) {
            return Some(r);
        }
    }
    all.into_iter().next()
}

pub fn available(dirs: &Dirs) -> bool {
    pick(dirs, &home()).is_some()
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn script(r: &Runner, server: bool) -> String {
    let bin = if server { &r.wineserver } else { &r.wine };
    match &r.root {
        None => format!("#!/bin/sh\n# generated by ustan\nexec \"{}\" \"$@\"\n", bin.display()),
        Some(root) => {
            let libs: Vec<String> = ["lib64", "lib"].iter().map(|d| root.join(d)).filter(|p| p.is_dir()).map(|p| p.display().to_string()).collect();
            let dlls: Vec<String> = ["lib64/wine", "lib/wine"].iter().map(|d| root.join(d)).filter(|p| p.is_dir()).map(|p| p.display().to_string()).collect();
            format!(
                "#!/bin/sh\n# generated by ustan (runner: {})\nexport LD_LIBRARY_PATH=\"{}${{LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}}\"\nexport WINEDLLPATH=\"{}${{WINEDLLPATH:+:$WINEDLLPATH}}\"\nexec \"{}\" \"$@\"\n",
                r.name,
                libs.join(":"),
                dlls.join(":"),
                bin.display()
            )
        }
    }
}

fn write_exec(path: &Path, text: &str) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    if std::fs::read_to_string(path).ok().as_deref() != Some(text) {
        std::fs::write(path, text)?;
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// (Re)write the wrapper scripts for `r` and return `(wine, wineserver)` wrapper paths.
pub fn write_wrappers(dirs: &Dirs, r: &Runner) -> Result<(PathBuf, PathBuf)> {
    let d = dirs.state.join("runner");
    let (w, s) = (d.join("wine"), d.join("wineserver"));
    write_exec(&w, &script(r, false))?;
    write_exec(&s, &script(r, true))?;
    Ok((w, s))
}

/// Wrappers for the chosen runner, or an error that says what to install.
pub fn ensure(dirs: &Dirs) -> Result<(PathBuf, PathBuf, Runner)> {
    let r = pick(dirs, &home()).ok_or_else(|| {
        Error::Format("не знайшов Wine: ні `wine` у PATH, ні PortProton, ні Steam Proton (див. `ustan runner`)".into())
    })?;
    let (w, s) = write_wrappers(dirs, &r)?;
    Ok((w, s, r))
}

/// Remember the user's choice (a runner name from [`discover`] or a path to `wine`).
pub fn set(dirs: &Dirs, choice: &str) -> Result<Runner> {
    let known = discover(&home());
    let r = known
        .iter()
        .find(|r| r.name == choice)
        .cloned()
        .or_else(|| Path::new(choice).parent().and_then(Path::parent).and_then(|root| from_root(choice.to_string(), root, 0)))
        .ok_or_else(|| Error::Format(format!("невідомий runner `{choice}` (див. `ustan runner`)")))?;
    let mut c = crate::config::load(dirs);
    c.wine = Some(choice.to_string());
    crate::config::save(dirs, &c)?;
    write_wrappers(dirs, &r)?;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_root(base: &Path, rel: &str) -> PathBuf {
        let r = base.join(rel);
        std::fs::create_dir_all(r.join("bin")).unwrap();
        std::fs::create_dir_all(r.join("lib64/wine")).unwrap();
        std::fs::write(r.join("bin/wine64"), "").unwrap();
        std::fs::write(r.join("bin/wineserver"), "").unwrap();
        r
    }

    #[test]
    fn discovers_and_ranks_runners_and_writes_wrappers() {
        let h = std::env::temp_dir().join(format!("ustan-rn-{}", std::process::id()));
        fake_root(&h, ".var/app/ru.linux_gaming.PortProton/data/dist/WINE_LG_11-1");
        fake_root(&h, ".var/app/ru.linux_gaming.PortProton/data/dist/PROTON_LG_10-28");
        fake_root(&h, ".local/share/Steam/steamapps/common/Proton 10.0/files");
        std::fs::create_dir_all(h.join(".local/share/Steam/steamapps/common/Proton Hotfix")).unwrap();
        let found = discover(&h);
        let names: Vec<_> = found.iter().filter(|r| r.root.is_some()).map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["portproton/WINE_LG_11-1", "portproton/PROTON_LG_10-28", "proton/Proton 10.0"]);

        let dirs = Dirs::under(&h);
        let (w, s) = write_wrappers(&dirs, &found.iter().find(|r| r.root.is_some()).unwrap().clone()).unwrap();
        let text = std::fs::read_to_string(&w).unwrap();
        assert!(text.contains("LD_LIBRARY_PATH") && text.contains("WINEDLLPATH") && text.contains("WINE_LG_11-1/bin/wine64"), "{text}");
        assert!(std::fs::read_to_string(&s).unwrap().contains("bin/wineserver"));
        assert!(std::fs::metadata(&w).unwrap().permissions().mode() & 0o111 != 0);
        let _ = std::fs::remove_dir_all(h);
    }
}
