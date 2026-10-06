use crate::{Error, Result};

extern "C" {
    fn ustan_elf_needed(data: *const u8, len: usize, out: *mut u8, cap: usize, out_len: *mut usize) -> i32;
    fn ustan_elf_section(data: *const u8, len: usize, name: *const u8, name_len: usize, off: *mut u64, size: *mut u64) -> i32;
    fn ustan_elf_end(data: *const u8, len: usize, out: *mut u64) -> i32;
}

/// Offset where an ELF64-LE file's own contents end (AppImage squashfs starts here).
pub fn end(data: &[u8]) -> Result<u64> {
    let mut out = 0u64;
    match unsafe { ustan_elf_end(data.as_ptr(), data.len(), &mut out) } {
        0 => Ok(out),
        -1 => Err(Error::Format("not an ELF file".into())),
        -2 => Err(Error::Format("only ELF64 little-endian is supported".into())),
        _ => Err(Error::Format("truncated ELF file".into())),
    }
}

/// Byte range of a named section, or `None` when the file has no such section.
pub fn section(data: &[u8], name: &str) -> Option<std::ops::Range<usize>> {
    let (mut off, mut len) = (0u64, 0u64);
    let rc = unsafe { ustan_elf_section(data.as_ptr(), data.len(), name.as_ptr(), name.len(), &mut off, &mut len) };
    (rc == 0).then(|| off as usize..(off + len) as usize)
}

/// Shared libraries (`DT_NEEDED`) of an ELF executable held in memory. (`.dynamic` often sits at the
/// *end* of big binaries, so the whole image is needed; use [`needed_of_file`] to avoid reading it.)
pub fn needed(data: &[u8]) -> Result<Vec<String>> {
    let mut buf = vec![0u8; 16 * 1024];
    let mut n = 0usize;
    match unsafe { ustan_elf_needed(data.as_ptr(), data.len(), buf.as_mut_ptr(), buf.len(), &mut n) } {
        0 => Ok(String::from_utf8_lossy(&buf[..n]).lines().map(str::to_string).collect()),
        -1 => Err(Error::Format("not an ELF file".into())),
        -2 => Err(Error::Format("only ELF64 little-endian is supported".into())),
        _ => Err(Error::Format("cannot read the dynamic section".into())),
    }
}

/// Like [`needed`], for a file on disk: it is memory-mapped, only the touched pages are read.
pub fn needed_of_file(path: &std::path::Path) -> Result<Vec<String>> {
    let f = std::fs::File::open(path)?;
    // SAFETY: read-only mapping; a concurrent truncation could fault, which we accept for local files.
    let map = unsafe { memmap2::Mmap::map(&f)? };
    needed(&map)
}

#[cfg(test)]
mod tests {
    #[test]
    fn needed_libs_of_a_real_executable() {
        // the test binary is a dynamically linked ELF: it must at least need libc
        let libs = super::needed_of_file(&std::env::current_exe().unwrap()).unwrap();
        assert!(libs.iter().any(|l| l.starts_with("libc.so")), "{libs:?}");
        assert!(super::needed(b"not an elf at all, really not one, no").is_err());
    }
}
