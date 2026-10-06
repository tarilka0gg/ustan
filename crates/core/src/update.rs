//! Update checks and in-place updates of installed apps.
//!
//! Sources, in order: Flatpak (asks flatpak), AppImage `.upd_info` pointing at GitHub Releases,
//! and anything installed from a URL (HTTP validator changed = new file).
use crate::{backend, dirs::Dirs, fetch, manifest::Manifest, Error, Result};
use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    UpToDate,
    /// A newer version exists; the string is a short human label.
    Available(String),
    /// We have no way to know (no update info, no URL, offline...).
    Unknown(String),
}

struct Release {
    /// What identifies this release (tag, or asset timestamp for rolling tags).
    marker: String,
    url: String,
    /// GitHub publishes `sha256:<hex>` for release assets; used to verify the download.
    sha256: Option<String>,
}

fn glob(pat: &str, s: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == s;
    }
    let mut rest = s;
    for (i, p) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(p) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(p);
        } else {
            match rest.find(p) {
                Some(at) => rest = &rest[at + p.len()..],
                None => return false,
            }
        }
    }
    true
}

fn norm(v: &str) -> &str {
    v.trim().trim_start_matches(['v', 'V'])
}

fn gh_release(info: &str) -> Result<(Release, bool)> {
    let p: Vec<&str> = info.split('|').collect();
    if p.len() < 5 || p[0] != "gh-releases-zsync" {
        return Err(Error::Format(format!("unsupported update info: {info}")));
    }
    let (owner, repo, tag, pattern) = (p[1], p[2], p[3], p[4].trim_end_matches(".zsync"));
    let latest = tag == "latest";
    let url = if latest {
        format!("https://api.github.com/repos/{owner}/{repo}/releases/latest")
    } else {
        format!("https://api.github.com/repos/{owner}/{repo}/releases/tags/{tag}")
    };
    let resp = ureq::get(&url)
        .set("User-Agent", "ustan")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| Error::Format(format!("github: {e}")))?;
    let j: serde_json::Value = resp.into_json().map_err(|e| Error::Format(format!("github json: {e}")))?;
    let asset = j["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["name"].as_str().is_some_and(|n| glob(pattern, n))))
        .ok_or_else(|| Error::Format(format!("no release asset matches `{pattern}`")))?;
    let marker = if latest { j["tag_name"].as_str() } else { asset["updated_at"].as_str() }
        .ok_or_else(|| Error::Format("release has no marker".into()))?
        .to_string();
    let dl = asset["browser_download_url"].as_str().ok_or_else(|| Error::Format("asset has no url".into()))?.to_string();
    let sha256 = asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(str::to_string);
    Ok((Release { marker, url: dl, sha256 }, latest))
}

fn flatpak_has_update(app: &str) -> Result<bool> {
    let o = Command::new("flatpak")
        .args(["remote-ls", "--user", "--updates", "--columns=application"])
        .output()
        .map_err(|e| Error::Format(format!("cannot run flatpak: {e}")))?;
    Ok(String::from_utf8_lossy(&o.stdout).lines().any(|l| l.trim() == app))
}

pub fn check(m: &Manifest) -> Result<Status> {
    if m.kind == "snap" {
        // the store is the source of truth for snaps, whether installed from it or from a file
        return Ok(match backend::snap::store_info(&m.name) {
            Ok(i) if m.version.as_deref() == Some(i.version.as_str()) => Status::UpToDate,
            Ok(i) => Status::Available(i.version),
            Err(e) => Status::Unknown(e.to_string()),
        });
    }
    if m.kind == "flatpak" {
        return Ok(if flatpak_has_update(&m.name)? { Status::Available("нова версія".into()) } else { Status::UpToDate });
    }
    if let Some(info) = &m.update_info {
        let (rel, latest) = gh_release(info)?;
        return Ok(match (&m.remote_version, latest) {
            (Some(seen), _) if *seen == rel.marker => Status::UpToDate,
            (Some(_), _) => Status::Available(label(&rel.marker, latest)),
            (None, true) => match &m.version {
                Some(v) if norm(v) == norm(&rel.marker) => Status::UpToDate,
                Some(_) => Status::Available(label(&rel.marker, true)),
                None => Status::Unknown("невідома встановлена версія".into()),
            },
            (None, false) => Status::Unknown("немає бази для порівняння".into()),
        });
    }
    if let Some(url) = &m.url {
        let now = fetch::head_validator(url)?;
        return Ok(match (&m.etag, now) {
            (Some(old), Some(new)) if *old == new => Status::UpToDate,
            (Some(_), Some(_)) => Status::Available("файл змінився".into()),
            _ => Status::Unknown("сервер не повідомляє версію".into()),
        });
    }
    Ok(Status::Unknown("джерело оновлень невідоме".into()))
}

fn label(marker: &str, is_tag: bool) -> String {
    if is_tag { norm(marker).to_string() } else { "нова збірка".into() }
}

/// Replace the installed copy with a freshly downloaded file. The new file is inspected *before*
/// the old copy is removed, so a bad download never destroys a working install.
fn replace_from_file(m: &Manifest, file: &std::path::Path, dirs: &Dirs) -> Result<Manifest> {
    let b = backend::pick(file).ok_or_else(|| Error::Format("downloaded file type is not supported".into()))?;
    b.inspect(file)?;
    m.uninstall(&dirs.state)?;
    let mut new = b.install(file, dirs, &backend::Opts::default())?;
    if m.runtime || !m.used_by.is_empty() {
        // an updated runtime stays a runtime and keeps its users (they point at the same directory)
        new.runtime = m.runtime;
        new.used_by = m.used_by.clone();
        new.save(&dirs.state)?;
    }
    Ok(new)
}

pub fn apply(m: &Manifest, dirs: &Dirs) -> Result<Manifest> {
    let cache = dirs.state.join("cache");
    if m.kind == "flatpak" {
        let st = Command::new("flatpak")
            .args(["update", "--user", "-y", "--noninteractive", &m.name])
            .status()
            .map_err(|e| Error::Format(format!("cannot run flatpak: {e}")))?;
        return if st.success() { Ok(m.clone()) } else { Err(Error::Format(format!("flatpak update failed: {st}"))) };
    }
    if m.kind == "snap" {
        let i = backend::snap::store_info(&m.name)?;
        let file = fetch::download_checked(&i.url, &cache, &fetch::Expect::Sha3_384(i.sha3_384), &m.name)?.path;
        let res = replace_from_file(m, &file, dirs);
        let _ = std::fs::remove_file(&file);
        return res;
    }
    if let Some(info) = &m.update_info {
        let (rel, _) = gh_release(info)?;
        let file = fetch::download_checked(&rel.url, &cache, &rel.sha256.clone().map(fetch::Expect::Sha256).unwrap_or_default(), &m.name)?.path;
        let res = replace_from_file(m, &file, dirs);
        let _ = std::fs::remove_file(&file);
        let mut new = res?;
        new.remote_version = Some(rel.marker);
        new.save(&dirs.state)?;
        return Ok(new);
    }
    if let Some(url) = &m.url {
        let got = fetch::download_auto(url, &cache, &m.name)?;
        let (file, etag) = (got.path, got.validator);
        let res = replace_from_file(m, &file, dirs);
        let _ = std::fs::remove_file(&file);
        let mut new = res?;
        new.url = Some(url.clone());
        new.etag = etag;
        new.save(&dirs.state)?;
        return Ok(new);
    }
    Err(Error::Format("для цієї програми невідоме джерело оновлень".into()))
}

/// Update check for an unmanaged AppImage found on disk.
pub fn check_found(f: &crate::discover::Found) -> Result<Status> {
    let m = Manifest { kind: "appimage".into(), version: f.version.clone(), update_info: f.update_info.clone(), ..Default::default() };
    check(&m)
}

/// Replace an unmanaged AppImage in place with the latest release (same path, same permissions).
/// Returns the new version if the file declares one.
pub fn apply_found(f: &crate::discover::Found, dirs: &Dirs) -> Result<Option<String>> {
    use std::os::unix::fs::PermissionsExt;
    let info = f.update_info.as_deref().ok_or_else(|| Error::Format("у цього AppImage немає інформації про оновлення".into()))?;
    let (rel, _) = gh_release(info)?;
    let file = fetch::download_checked(&rel.url, &dirs.state.join("cache"), &rel.sha256.clone().map(fetch::Expect::Sha256).unwrap_or_default(), &f.name)?.path;
    let res = (|| -> Result<Option<String>> {
        if !backend::appimage::is_appimage(&file) {
            return Err(Error::Format("завантажений файл не є AppImage".into()));
        }
        let probe = backend::appimage::probe(&file)?;
        let mode = std::fs::metadata(&f.path)?.permissions().mode() | 0o111;
        let tmp = f.path.with_extension("AppImage.ustan-new");
        std::fs::copy(&file, &tmp)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
        // Atomic on the same filesystem; a running copy keeps its old inode.
        std::fs::rename(&tmp, &f.path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
        Ok(probe.version)
    })();
    let _ = std::fs::remove_file(&file);
    res
}

#[cfg(test)]
mod tests {
    use super::glob;

    #[test]
    fn globs() {
        assert!(glob("App-*x86_64.AppImage", "App-1.2-x86_64.AppImage"));
        assert!(!glob("App-*x86_64.AppImage", "Other-1.2-x86_64.AppImage"));
        assert!(glob("exact", "exact"));
        assert!(!glob("a*b", "a"));
    }
}
