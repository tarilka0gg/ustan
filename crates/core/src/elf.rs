use crate::{Error, Result};

extern "C" {
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
