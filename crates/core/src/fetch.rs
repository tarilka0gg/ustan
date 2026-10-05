//! Download a package from the internet into the cache, optionally verifying sha256.
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

/// Validator of a remote file (ETag preferred, else Last-Modified): changes when the file changes.
pub fn validator(resp: &ureq::Response) -> Option<String> {
    resp.header("ETag").or_else(|| resp.header("Last-Modified")).map(str::to_string)
}

/// Current validator of `url` without downloading the body.
pub fn head_validator(url: &str) -> Result<Option<String>> {
    let resp = ureq::head(url).call().map_err(|e| Error::Format(format!("head: {e}")))?;
    Ok(validator(&resp))
}

/// Download `url` into `cache`, returning the file path. Fails (and deletes the file) on hash mismatch.
pub fn download(url: &str, cache: &Path, sha256: Option<&str>) -> Result<PathBuf> {
    download_with_validator(url, cache, sha256).map(|(p, _)| p)
}

pub fn download_with_validator(url: &str, cache: &Path, sha256: Option<&str>) -> Result<(PathBuf, Option<String>)> {
    std::fs::create_dir_all(cache)?;
    let dst = cache.join(file_name(url));
    let resp = ureq::get(url).call().map_err(|e| Error::Format(format!("download: {e}")))?;
    let etag = validator(&resp);
    let mut r = resp.into_reader();
    let mut f = std::fs::File::create(&dst)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        f.write_all(&buf[..n])?;
    }
    let got: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if let Some(want) = sha256 {
        if !got.eq_ignore_ascii_case(want.trim()) {
            let _ = std::fs::remove_file(&dst);
            return Err(Error::Format(format!("sha256 mismatch: expected {want}, got {got}")));
        }
    }
    Ok((dst, etag))
}

#[cfg(test)]
mod tests {
    #[test]
    fn names() {
        assert_eq!(super::file_name("https://x.org/a/b/App-1.0.AppImage?dl=1"), "App-1.0.AppImage");
        assert_eq!(super::file_name("https://x.org/"), "download");
        assert_eq!(super::file_name("https://x.org/.."), "download");
    }
}
