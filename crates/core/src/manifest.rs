//! Per-app install record, used for clean uninstall.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub kind: String,
    pub source: Option<String>,
    /// Every file/dir we created; removed in reverse order on uninstall.
    pub files: Vec<PathBuf>,
    /// Extra command run on uninstall (e.g. `flatpak uninstall`).
    #[serde(default)]
    pub uninstall_cmd: Vec<String>,
    /// Download URL when installed from the internet (enables update checks).
    #[serde(default)]
    pub url: Option<String>,
    /// HTTP validator (ETag or Last-Modified) of what was installed from `url`.
    #[serde(default)]
    pub etag: Option<String>,
    /// AppImage update information (`gh-releases-zsync|owner|repo|tag|pattern`).
    #[serde(default)]
    pub update_info: Option<String>,
    /// Release marker we last installed from `update_info` (tag name).
    #[serde(default)]
    pub remote_version: Option<String>,
    /// Things worth telling the user after install (e.g. libraries the program needs but the system lacks).
    #[serde(default)]
    pub notes: Vec<String>,
}


fn path_for(dir: &Path, id: &str) -> PathBuf {
    dir.join("installed").join(format!("{id}.toml"))
}

impl Manifest {
    pub fn save(&self, dir: &Path) -> Result<()> {
        let p = path_for(dir, &self.id);
        std::fs::create_dir_all(p.parent().unwrap())?;
        let s = toml::to_string_pretty(self).map_err(|e| Error::Manifest(e.to_string()))?;
        std::fs::write(p, s)?;
        Ok(())
    }

    pub fn load(dir: &Path, id: &str) -> Result<Self> {
        let s = std::fs::read_to_string(path_for(dir, id))?;
        toml::from_str(&s).map_err(|e| Error::Manifest(e.to_string()))
    }

    pub fn list(dir: &Path) -> Result<Vec<Self>> {
        let d = dir.join("installed");
        let mut v = Vec::new();
        if d.is_dir() {
            for e in std::fs::read_dir(d)? {
                let s = std::fs::read_to_string(e?.path())?;
                v.push(toml::from_str(&s).map_err(|e| Error::Manifest(e.to_string()))?);
            }
        }
        v.sort_by(|a: &Self, b: &Self| a.id.cmp(&b.id));
        Ok(v)
    }

    pub fn uninstall(&self, dir: &Path) -> Result<()> {
        if let Some((prog, args)) = self.uninstall_cmd.split_first() {
            let st = std::process::Command::new(prog).args(args).status()?;
            if !st.success() {
                return Err(Error::Format(format!("`{prog}` failed: {st}")));
            }
        }
        for f in self.files.iter().rev() {
            if f.is_dir() {
                let _ = std::fs::remove_dir_all(f);
            } else if f.exists() {
                std::fs::remove_file(f)?;
            }
        }
        let _ = std::fs::remove_file(path_for(dir, &self.id));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_and_uninstall() {
        let d = std::env::temp_dir().join(format!("ustan-t-{}", std::process::id()));
        let f = d.join("x.txt");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(&f, "hi").unwrap();
        let m = Manifest { id: "a".into(), name: "A".into(), version: None, kind: "test".into(), source: None, files: vec![f.clone()], uninstall_cmd: vec![], ..Default::default() };
        m.save(&d).unwrap();
        assert_eq!(Manifest::list(&d).unwrap().len(), 1);
        Manifest::load(&d, "a").unwrap().uninstall(&d).unwrap();
        assert!(!f.exists());
        assert!(Manifest::list(&d).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(d);
    }
}
