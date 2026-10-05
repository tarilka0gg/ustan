//! Thin managed layer over the `flatpak` CLI (per-user installation).
//! Accepts `flatpak:<app-id>` (from Flathub), `*.flatpakref` and `*.flatpak` bundles.
use super::{slug, Backend, Info, Opts};
use crate::{dirs::Dirs, manifest::Manifest, Error, Result};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

pub struct Flatpak;

enum Source {
    Remote(String),
    Ref(String),
    Bundle(String),
}

fn source(path: &Path) -> Option<Source> {
    let s = path.to_string_lossy();
    if let Some(id) = s.strip_prefix("flatpak:") {
        return Some(Source::Remote(id.to_string()));
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some("flatpakref") => Some(Source::Ref(s.into_owned())),
        Some("flatpak") => Some(Source::Bundle(s.into_owned())),
        _ => None,
    }
}

fn flatpak(args: &[&str]) -> Result<std::process::Output> {
    Command::new("flatpak").args(args).output().map_err(|e| Error::Format(format!("cannot run flatpak: {e}")))
}

fn installed_apps() -> Result<BTreeSet<String>> {
    let o = flatpak(&["list", "--user", "--app", "--columns=application"])?;
    Ok(String::from_utf8_lossy(&o.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

fn ref_field(path: &str, key: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()?.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')).map(|v| v.trim().to_string())
}

impl Backend for Flatpak {
    fn kind(&self) -> &'static str {
        "flatpak"
    }

    fn detect(&self, path: &Path) -> bool {
        source(path).is_some()
    }

    fn inspect(&self, path: &Path) -> Result<Info> {
        match source(path).ok_or_else(|| Error::Format("not a flatpak source".into()))? {
            Source::Remote(id) => Ok(Info { id: slug(&id), name: id, version: None, kind: "flatpak", icon: None }),
            Source::Ref(f) => {
                let id = ref_field(&f, "Name").ok_or_else(|| Error::Format("flatpakref has no Name".into()))?;
                let name = ref_field(&f, "Title").unwrap_or_else(|| id.clone());
                Ok(Info { id: slug(&id), name, version: ref_field(&f, "Branch"), kind: "flatpak", icon: None })
            }
            Source::Bundle(f) => {
                let n = Path::new(&f).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                Ok(Info { id: slug(&n), name: n, version: None, kind: "flatpak", icon: None })
            }
        }
    }

    fn install(&self, path: &Path, dirs: &Dirs, _opts: &Opts) -> Result<Manifest> {
        let src = source(path).ok_or_else(|| Error::Format("not a flatpak source".into()))?;
        let before = installed_apps()?;
        let args: Vec<&str> = match &src {
            Source::Remote(id) => vec!["install", "--user", "-y", "--noninteractive", "flathub", id],
            Source::Ref(f) => vec!["install", "--user", "-y", "--noninteractive", f],
            Source::Bundle(f) => vec!["install", "--user", "-y", "--noninteractive", "--bundle", f],
        };
        // Inherit stdio so the user sees flatpak's own progress.
        let st = Command::new("flatpak").args(&args).status().map_err(|e| Error::Format(format!("cannot run flatpak: {e}")))?;
        if !st.success() {
            return Err(Error::Format(format!("flatpak install failed: {st}")));
        }
        let after = installed_apps()?;
        let app = match &src {
            Source::Remote(id) => id.clone(),
            _ => after
                .difference(&before)
                .next()
                .cloned()
                .ok_or_else(|| Error::Format("flatpak installed nothing new (already installed?)".into()))?,
        };
        let m = Manifest {
            id: slug(&app),
            name: app.clone(),
            version: None,
            kind: "flatpak".into(),
            source: Some(path.display().to_string()),
            files: vec![],
            uninstall_cmd: ["flatpak", "uninstall", "--user", "-y", "--noninteractive", &app].map(String::from).to_vec(),
        };
        m.save(&dirs.state)?;
        Ok(m)
    }
}
