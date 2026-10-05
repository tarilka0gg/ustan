//! `.jar` applications: run with `java -jar`. A jar with an icon becomes a menu launcher,
//! one without is treated as a command-line tool and gets a script in `~/.local/bin`.
use super::tree::{which, install_tree, Spec};
use super::{slug, Backend, Icon, Info, Opts};
use crate::{dirs::Dirs, manifest::Manifest, Error, Result};
use std::io::{Cursor, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub struct Jar;

struct Meta {
    name: String,
    version: Option<String>,
    icon: Option<Icon>,
}

/// `Key: value` pairs of a MANIFEST.MF (continuation lines start with a space).
fn manifest_attrs(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for l in text.lines() {
        if let Some(cont) = l.strip_prefix(' ') {
            if let Some(last) = out.last_mut() {
                last.1.push_str(cont);
            }
        } else if let Some((k, v)) = l.split_once(": ") {
            out.push((k.to_string(), v.trim().to_string()));
        }
    }
    out
}

fn attr<'a>(attrs: &'a [(String, String)], k: &str) -> Option<&'a str> {
    attrs.iter().find(|(a, _)| a.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
}

fn read_meta(path: &Path, bytes: &[u8]) -> Result<Meta> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| Error::Format(format!("не JAR/zip: {e}")))?;
    let mf = {
        let mut f = z.by_name("META-INF/MANIFEST.MF").map_err(|_| Error::Format("немає META-INF/MANIFEST.MF".into()))?;
        let mut s = String::new();
        f.read_to_string(&mut s)?;
        s
    };
    let a = manifest_attrs(&mf);
    if attr(&a, "Main-Class").is_none() {
        return Err(Error::Format("у JAR немає Main-Class: це бібліотека, а не програма".into()));
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "app".into());
    let name = ["Application-Name", "Implementation-Title", "Bundle-Name", "Specification-Title"]
        .iter()
        .find_map(|k| attr(&a, k))
        .map(str::to_string)
        .unwrap_or_else(|| stem.clone());
    let version = ["Implementation-Version", "Bundle-Version", "Specification-Version"].iter().find_map(|k| attr(&a, k)).map(str::to_string);

    // The best icon-like image: names with icon/logo, preferring svg, then the biggest file.
    let mut best: Option<(u64, String)> = None;
    for i in 0..z.len() {
        let Ok(f) = z.by_index(i) else { continue };
        let n = f.name().to_lowercase();
        let img = n.ends_with(".png") || n.ends_with(".svg");
        if img && (n.contains("icon") || n.contains("logo")) && f.size() < 2_000_000 {
            let score = if n.ends_with(".svg") { u64::MAX } else { f.size() };
            if best.as_ref().is_none_or(|b| score > b.0) {
                best = Some((score, f.name().to_string()));
            }
        }
    }
    let icon = best.and_then(|(_, n)| {
        let mut f = z.by_name(&n).ok()?;
        let mut b = Vec::new();
        f.read_to_end(&mut b).ok()?;
        Some(Icon { ext: if n.to_lowercase().ends_with(".svg") { "svg" } else { "png" }, bytes: b })
    });
    Ok(Meta { name, version, icon })
}

impl Backend for Jar {
    fn kind(&self) -> &'static str {
        "jar"
    }

    fn detect(&self, path: &Path) -> bool {
        path.extension().is_some_and(|e| e.eq_ignore_ascii_case("jar"))
    }

    fn inspect(&self, path: &Path) -> Result<Info> {
        let m = read_meta(path, &std::fs::read(path)?)?;
        let warning = which("java").is_none().then(|| "потрібна Java (команда `java` не знайдена в PATH)".to_string());
        Ok(Info { id: slug(&m.name), name: m.name, version: m.version, kind: "jar", icon: m.icon, warning })
    }

    fn install(&self, path: &Path, dirs: &Dirs, opts: &Opts) -> Result<Manifest> {
        let bytes = std::fs::read(path)?;
        let m = read_meta(path, &bytes)?;
        let file = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "app.jar".into());
        let spec = Spec { kind: "jar", id: slug(&m.name), name: m.name.clone(), version: m.version.clone(), source: path };
        install_tree(
            spec,
            dirs,
            opts,
            |root| {
                std::fs::write(root.join(&file), &bytes)?;
                if let Some(i) = &m.icon {
                    std::fs::write(root.join(format!("icon.{}", i.ext)), &i.bytes)?;
                }
                Ok(())
            },
            |root, dirs, id, files| {
                let jar = root.join(&file);
                if let Some(i) = &m.icon {
                    std::fs::create_dir_all(&dirs.apps)?;
                    let t = format!(
                        "[Desktop Entry]\nType=Application\nName={}\nExec=java -jar \"{}\"\nPath={}\nTerminal=false\nCategories=Utility;\nIcon={}\n",
                        m.name,
                        jar.display(),
                        root.display(),
                        root.join(format!("icon.{}", i.ext)).display()
                    );
                    let dst = dirs.apps.join(format!("ustan-{id}.desktop"));
                    std::fs::write(&dst, t)?;
                    files.push(dst);
                } else {
                    // no icon: a command-line tool
                    let bin = dirs.opt.parent().unwrap_or(&dirs.opt).join("bin");
                    std::fs::create_dir_all(&bin)?;
                    let script = bin.join(id);
                    if std::fs::symlink_metadata(&script).is_ok() {
                        return Err(Error::Format(format!("{} уже існує в {}", id, bin.display())));
                    }
                    std::fs::write(&script, format!("#!/bin/sh\nexec java -jar \"{}\" \"$@\"\n", jar.display()))?;
                    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
                    files.push(script);
                }
                Ok(())
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_parsing_with_continuations() {
        let a = manifest_attrs("Manifest-Version: 1.0\nMain-Class: com.example.Very\n LongName\nImplementation-Title: Demo App\n");
        assert_eq!(attr(&a, "main-class"), Some("com.example.VeryLongName"));
        assert_eq!(attr(&a, "Implementation-Title"), Some("Demo App"));
        assert_eq!(attr(&a, "Nope"), None);
    }
}
