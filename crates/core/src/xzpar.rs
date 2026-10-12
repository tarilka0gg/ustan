//! Multi-threaded `.xz` reading (the work is done in Zig, see `zig/src/xzpar.zig`).
use std::io::{self, Read};
use std::path::Path;

#[repr(C)]
struct Raw {
    _p: [u8; 0],
}

extern "C" {
    fn ustan_xz_open(path: *const u8, len: usize, threads: usize) -> *mut Raw;
    fn ustan_xz_read(r: *mut Raw, out: *mut u8, cap: usize) -> isize;
    fn ustan_xz_close(r: *mut Raw);
}

pub struct XzPar(*mut Raw);

// the Zig side guards all its state with a mutex
unsafe impl Send for XzPar {}

impl XzPar {
    /// `None` when the file is not a multi-Block single-Stream xz: use a sequential decoder then.
    pub fn open(path: &Path) -> Option<XzPar> {
        use std::os::unix::ffi::OsStrExt;
        let b = path.as_os_str().as_bytes();
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).min(6);
        let p = unsafe { ustan_xz_open(b.as_ptr(), b.len(), threads) };
        if p.is_null() { None } else { Some(XzPar(p)) }
    }
}

impl Read for XzPar {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match unsafe { ustan_xz_read(self.0, buf.as_mut_ptr(), buf.len()) } {
            n if n >= 0 => Ok(n as usize),
            _ => Err(io::Error::new(io::ErrorKind::InvalidData, "xz: damaged block")),
        }
    }
}

impl Drop for XzPar {
    fn drop(&mut self) {
        unsafe { ustan_xz_close(self.0) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn multi_block(data: &[u8], block: u64) -> Vec<u8> {
        let mut b = liblzma::stream::MtStreamBuilder::new();
        b.threads(2).block_size(block).preset(1);
        let s = b.encoder().unwrap();
        let mut w = liblzma::write::XzEncoder::new_stream(Vec::new(), s);
        w.write_all(data).unwrap();
        w.finish().unwrap()
    }

    #[test]
    fn parallel_read_equals_sequential() {
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8 ^ (i / 1000) as u8).collect();
        let dir = tempfile_dir();
        let f = dir.join("a.xz");
        std::fs::write(&f, multi_block(&data, 200_000)).unwrap();
        let mut got = Vec::new();
        XzPar::open(&f).expect("multi-block xz").read_to_end(&mut got).unwrap();
        assert!(got == data);
        // a single Block is left to the sequential decoder
        let one = dir.join("b.xz");
        std::fs::write(&one, multi_block(&data[..1000], 1 << 20)).unwrap();
        assert!(XzPar::open(&one).is_none());
        assert!(XzPar::open(&dir.join("missing.xz")).is_none());
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ustan-xzpar-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
