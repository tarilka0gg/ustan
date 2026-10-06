//! Download a package from the internet into the cache, verifying a checksum when one is known.
use crate::progress::{self, Event};
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

fn file_name(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let n = path.rsplit('/').next().unwrap_or("");
    let n: String = n.chars().filter(|c| c.is_ascii_alphanumeric() || "._-+~".contains(*c)).collect();
    if n.is_empty() || n.starts_with('.') { "download".into() } else { n }
}

/// What the file is expected to hash to. The snap store publishes SHA3-384.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Expect {
    #[default]
    None,
    Sha256(String),
    Sha3_384(String),
}

#[derive(Debug)]
pub struct Fetched {
    pub path: PathBuf,
    /// ETag / Last-Modified of what we got (for update checks).
    pub validator: Option<String>,
    /// Which checksum matched, if any was available.
    pub verified: Option<&'static str>,
}

/// Validator of a remote file (ETag preferred, else Last-Modified): changes when the file changes.
pub fn validator(resp: &ureq::Response) -> Option<String> {
    resp.header("ETag").or_else(|| resp.header("Last-Modified")).map(str::to_string)
}

/// Current validator of `url` without downloading the body.
pub fn head_validator(url: &str) -> Result<Option<String>> {
    let resp = ureq::head(url).call().map_err(|e| Error::Format(format!("head: {e}")))?;
    Ok(validator(&resp))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

/// First 64-hex-digit token of a checksum file (`<hash>  <name>` or just `<hash>`).
fn parse_sha256(text: &str) -> Option<String> {
    text.split_whitespace().find(|t| t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit())).map(|t| t.to_lowercase())
}

/// A checksum published next to the file (`<url>.sha256`, `<url>.sha256sum`), if there is one.
pub fn sidecar_sha256(url: &str) -> Option<String> {
    let base = url.split(['?', '#']).next().unwrap_or(url);
    for ext in [".sha256", ".sha256sum"] {
        let Ok(resp) = ureq::get(&format!("{base}{ext}")).timeout(std::time::Duration::from_secs(10)).call() else { continue };
        let mut buf = String::new();
        if resp.into_reader().take(4096).read_to_string(&mut buf).is_ok() {
            if let Some(h) = parse_sha256(&buf) {
                return Some(h);
            }
        }
    }
    None
}

/// Download `url` into `cache`, report progress as `label`, honour cancel, and verify `expect`.
/// A mismatching or cancelled download is deleted.
pub fn download_checked(url: &str, cache: &Path, expect: &Expect, label: &str) -> Result<Fetched> {
    std::fs::create_dir_all(cache)?;
    let dst = cache.join(file_name(url));
    progress::check()?;
    let resp = ureq::get(url).call().map_err(|e| Error::Format(format!("завантаження: {e}")))?;
    let total = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok());
    let validator = validator(&resp);
    let mut r = resp.into_reader();
    let mut f = std::fs::File::create(&dst)?;
    let (mut s256, mut s3) = (Sha256::new(), sha3::Sha3_384::new());
    let want_sha3 = matches!(expect, Expect::Sha3_384(_));
    let (mut done, mut last_emit) = (0u64, 0u64);
    let mut buf = [0u8; 64 * 1024];
    let fail = |dst: &Path, e: Error| -> Error {
        let _ = std::fs::remove_file(dst);
        e
    };
    loop {
        if progress::cancelled() {
            return Err(fail(&dst, Error::Cancelled));
        }
        let n = r.read(&mut buf).map_err(|e| fail(&dst, e.into()))?;
        if n == 0 {
            break;
        }
        s256.update(&buf[..n]);
        if want_sha3 {
            sha3::Digest::update(&mut s3, &buf[..n]);
        }
        f.write_all(&buf[..n]).map_err(|e| fail(&dst, e.into()))?;
        done += n as u64;
        if done - last_emit >= 256 * 1024 {
            last_emit = done;
            progress::emit(Event::Download { name: label.to_string(), done, total });
        }
    }
    progress::emit(Event::Download { name: label.to_string(), done, total });

    let verified = match expect {
        Expect::None => None,
        Expect::Sha256(want) => {
            let got = hex(&s256.finalize());
            if !got.eq_ignore_ascii_case(want.trim()) {
                return Err(fail(&dst, Error::Format(format!("sha256 не збігається: очікувалось {want}, отримано {got}"))));
            }
            Some("sha256")
        }
        Expect::Sha3_384(want) => {
            let got = hex(&sha3::Digest::finalize(s3));
            if !got.eq_ignore_ascii_case(want.trim()) {
                return Err(fail(&dst, Error::Format(format!("sha3-384 не збігається: очікувалось {want}, отримано {got}"))));
            }
            Some("sha3-384")
        }
    };
    Ok(Fetched { path: dst, validator, verified })
}

/// Like [`download_checked`], but looks for a checksum published beside the file first.
pub fn download_auto(url: &str, cache: &Path, label: &str) -> Result<Fetched> {
    let expect = sidecar_sha256(url).map(Expect::Sha256).unwrap_or_default();
    download_checked(url, cache, &expect, label)
}

/// Download `url` into `cache`, returning the file path. Fails (and deletes the file) on hash mismatch.
pub fn download(url: &str, cache: &Path, sha256: Option<&str>) -> Result<PathBuf> {
    download_with_validator(url, cache, sha256).map(|(p, _)| p)
}

pub fn download_with_validator(url: &str, cache: &Path, sha256: Option<&str>) -> Result<(PathBuf, Option<String>)> {
    let expect = sha256.map(|h| Expect::Sha256(h.to_string())).unwrap_or_default();
    let f = download_checked(url, cache, &expect, &file_name(url))?;
    Ok((f.path, f.validator))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// One-shot HTTP server answering every request with `body`.
    fn serve(body: &'static [u8], requests: usize) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for _ in 0..requests {
                let Ok((mut s, _)) = l.accept() else { return };
                let mut req = [0u8; 2048];
                let _ = s.read(&mut req);
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(body);
            }
        });
        format!("http://127.0.0.1:{port}/file.bin")
    }

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ustan-f-{tag}-{}", std::process::id()))
    }

    #[test]
    fn names_and_checksum_files() {
        assert_eq!(file_name("https://x.org/a/b/App-1.0.AppImage?dl=1"), "App-1.0.AppImage");
        assert_eq!(file_name("https://x.org/"), "download");
        let h = "a".repeat(64);
        assert_eq!(parse_sha256(&format!("{h}  tool.tar.gz\n")), Some(h.clone()));
        assert_eq!(parse_sha256("not a hash"), None);
    }

    #[test]
    fn verifies_reports_progress_and_deletes_bad_downloads() {
        let _g = progress::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let url = serve(b"hello ustan", 3);
        let want = hex(&Sha256::digest(b"hello ustan"));
        let dir = tmp("ok");
        progress::reset();
        let events = Arc::new(Mutex::new(0));
        let e2 = events.clone();
        progress::set_hook(move |e| {
            if matches!(e, Event::Download { .. }) {
                *e2.lock().unwrap() += 1;
            }
        });

        let good = download_checked(&url, &dir, &Expect::Sha256(want), "test").unwrap();
        assert_eq!(good.verified, Some("sha256"));
        assert_eq!(std::fs::read(&good.path).unwrap(), b"hello ustan");
        assert!(*events.lock().unwrap() >= 1);

        let bad = download_checked(&url, &dir, &Expect::Sha256("0".repeat(64)), "test");
        assert!(matches!(bad, Err(Error::Format(m)) if m.contains("не збігається")));
        assert!(!dir.join("file.bin").exists(), "a mismatching download must not stay in the cache");

        progress::cancel();
        let c = download_checked(&url, &dir, &Expect::None, "test");
        assert!(matches!(c, Err(Error::Cancelled)));
        progress::reset();
        progress::clear_hook();
        let _ = std::fs::remove_dir_all(dir);
    }
}
