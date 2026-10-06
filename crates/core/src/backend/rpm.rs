//! `.rpm` packages: the header is parsed by the Zig library, the payload is a (compressed) cpio.
use super::tree::{decompress, dest, install_tree, remove_existing, safe_rel, Spec};
use super::{slug, Backend, Info, Opts};
use crate::{dirs::Dirs, manifest::Manifest, Error, Result};
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub struct Rpm;

extern "C" {
    fn ustan_rpm_payload(data: *const u8, len: usize, out: *mut u64) -> i32;
    fn ustan_rpm_string(data: *const u8, len: usize, tag: u32, buf: *mut u8, cap: usize, out_len: *mut usize) -> i32;
}

const NAME: u32 = 1000;
const VERSION: u32 = 1001;
const RELEASE: u32 = 1002;
const PAYLOADCOMPRESSOR: u32 = 1125;

fn tag(data: &[u8], tag: u32) -> Option<String> {
    let mut buf = vec![0u8; 1024];
    let mut n = 0usize;
    let rc = unsafe { ustan_rpm_string(data.as_ptr(), data.len(), tag, buf.as_mut_ptr(), buf.len(), &mut n) };
    (rc == 0).then(|| String::from_utf8_lossy(&buf[..n]).into_owned())
}

fn payload_offset(data: &[u8]) -> Result<usize> {
    let mut off = 0u64;
    match unsafe { ustan_rpm_payload(data.as_ptr(), data.len(), &mut off) } {
        0 => Ok(off as usize),
        -1 => Err(Error::Format("not an RPM file".into())),
        _ => Err(Error::Format("truncated RPM header".into())),
    }
}

struct Meta {
    name: String,
    version: String,
}

fn meta(data: &[u8]) -> Result<Meta> {
    payload_offset(data)?;
    let name = tag(data, NAME).ok_or_else(|| Error::Format("RPM has no name".into()))?;
    let v = tag(data, VERSION).unwrap_or_default();
    let version = match tag(data, RELEASE) {
        Some(r) if !v.is_empty() => format!("{v}-{r}"),
        _ => v,
    };
    Ok(Meta { name, version })
}

fn hex(b: &[u8]) -> Result<u32> {
    std::str::from_utf8(b).ok().and_then(|s| u32::from_str_radix(s, 16).ok()).ok_or_else(|| Error::Format("corrupt cpio header".into()))
}

/// Extract a "newc" cpio archive (what RPM uses) below `root`.
pub fn extract_cpio(data: &[u8], root: &Path) -> Result<()> {
    let canon_root = root.canonicalize()?;
    let mut pos = 0usize;
    // Hard links: every entry but the last of a group has no data; the last one carries it.
    let mut pending: HashMap<u32, Vec<PathBuf>> = HashMap::new();
    loop {
        if pos + 110 > data.len() {
            return Err(Error::Format("truncated cpio".into()));
        }
        let h = &data[pos..pos + 110];
        if &h[..5] != b"07070" || (h[5] != b'1' && h[5] != b'2') {
            return Err(Error::Format("unsupported cpio format".into()));
        }
        let (ino, mode, nlink) = (hex(&h[6..14])?, hex(&h[14..22])?, hex(&h[38..46])?);
        let (size, namesize) = (hex(&h[54..62])? as usize, hex(&h[94..102])? as usize);
        let name_end = pos + 110 + namesize;
        if namesize == 0 || name_end > data.len() {
            return Err(Error::Format("truncated cpio".into()));
        }
        let name = String::from_utf8_lossy(&data[pos + 110..name_end - 1]).into_owned();
        let body = (name_end + 3) & !3;
        let next = (body + size + 3) & !3;
        if name == "TRAILER!!!" {
            return Ok(());
        }
        if body + size > data.len() {
            return Err(Error::Format("truncated cpio".into()));
        }
        let contents = &data[body..body + size];
        pos = next;

        let Some(rel) = safe_rel(&name) else { continue };
        match mode & 0o170000 {
            0o040000 => {
                let d = dest(root, &canon_root, &rel)?;
                std::fs::create_dir_all(&d)?;
                std::fs::set_permissions(&d, std::fs::Permissions::from_mode((mode & 0o777) | 0o700))?;
            }
            0o100000 => {
                let d = dest(root, &canon_root, &rel)?;
                if nlink > 1 && size == 0 {
                    pending.entry(ino).or_default().push(d);
                    continue;
                }
                remove_existing(&d);
                std::fs::write(&d, contents)?;
                std::fs::set_permissions(&d, std::fs::Permissions::from_mode(mode & 0o777))?;
                for other in pending.remove(&ino).unwrap_or_default() {
                    remove_existing(&other);
                    if std::fs::hard_link(&d, &other).is_err() {
                        std::fs::copy(&d, &other)?;
                    }
                }
            }
            0o120000 => {
                let d = dest(root, &canon_root, &rel)?;
                remove_existing(&d);
                std::os::unix::fs::symlink(String::from_utf8_lossy(contents).as_ref(), &d)?;
            }
            _ => {} // devices, fifos, sockets: not for a per-user install
        }
    }
}

fn payload_name(data: &[u8]) -> &'static str {
    match tag(data, PAYLOADCOMPRESSOR).as_deref() {
        Some("xz") => ".xz",
        Some("zstd") => ".zst",
        Some("bzip2") => ".bz2",
        _ => ".gz",
    }
}

impl Backend for Rpm {
    fn kind(&self) -> &'static str {
        "rpm"
    }

    fn detect(&self, path: &Path) -> bool {
        path.extension().is_some_and(|e| e.eq_ignore_ascii_case("rpm"))
    }

    fn inspect(&self, path: &Path) -> Result<Info> {
        let data = std::fs::read(path)?;
        let m = meta(&data)?;
        Ok(Info { id: slug(&m.name), name: m.name, version: Some(m.version), kind: "rpm", icon: None, warning: None })
    }

    fn install(&self, path: &Path, dirs: &Dirs, opts: &Opts) -> Result<Manifest> {
        let data = std::fs::read(path)?;
        let m = meta(&data)?;
        let off = payload_offset(&data)?;
        let cpio = decompress(payload_name(&data), &data[off..])?;
        let spec = Spec { kind: "rpm", id: slug(&m.name), name: m.name, version: Some(m.version), source: path, overlay: true };
        install_tree(spec, dirs, opts, |root| extract_cpio(&cpio, root), |_, _, _, _| Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(out: &mut Vec<u8>, ino: u32, mode: u32, nlink: u32, name: &str, data: &[u8]) {
        let hdr = format!(
            "070701{ino:08X}{mode:08X}{:08X}{:08X}{nlink:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}",
            0, 0, 0, data.len(), 0, 0, 0, 0, name.len() + 1, 0
        );
        out.extend_from_slice(hdr.as_bytes());
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    #[test]
    fn extracts_files_links_and_blocks_escapes() {
        let mut c = Vec::new();
        entry(&mut c, 1, 0o040755, 2, "./usr", b"");
        entry(&mut c, 2, 0o040755, 2, "./usr/bin", b"");
        entry(&mut c, 3, 0o100755, 1, "./usr/bin/tool", b"#!/bin/sh\n");
        entry(&mut c, 4, 0o120777, 1, "./usr/bin/alias", b"tool");
        entry(&mut c, 5, 0o100644, 2, "./usr/a", b""); // hard link, data comes with the last entry
        entry(&mut c, 5, 0o100644, 2, "./usr/b", b"shared");
        entry(&mut c, 6, 0o100644, 1, "../escape", b"nope");
        entry(&mut c, 0, 0, 1, "TRAILER!!!", b"");

        let r = std::env::temp_dir().join(format!("ustan-cpio-{}", std::process::id()));
        std::fs::create_dir_all(&r).unwrap();
        extract_cpio(&c, &r).unwrap();
        assert_eq!(std::fs::read(r.join("usr/bin/tool")).unwrap(), b"#!/bin/sh\n");
        assert_eq!(std::fs::metadata(r.join("usr/bin/tool")).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(std::fs::read_link(r.join("usr/bin/alias")).unwrap(), PathBuf::from("tool"));
        assert_eq!(std::fs::read(r.join("usr/a")).unwrap(), b"shared");
        assert_eq!(std::fs::read(r.join("usr/b")).unwrap(), b"shared");
        assert!(!r.parent().unwrap().join("escape").exists());
        let _ = std::fs::remove_dir_all(r);
    }
}
