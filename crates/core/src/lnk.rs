use crate::{Error, Result};

extern "C" {
    fn ustan_lnk_parse(data: *const u8, len: usize, t: *mut u8, tcap: usize, a: *mut u8, acap: usize, tlen: *mut usize, alen: *mut usize) -> i32;
}

#[derive(Debug, PartialEq)]
pub struct Shortcut {
    /// Windows path of the target, e.g. `C:\Program Files\App\app.exe`.
    pub target: String,
    pub args: String,
}

/// Shortcut strings may carry NULs or other control characters (NSIS writes a lone NUL as the
/// "arguments"); they must never reach a .desktop file.
fn clean(b: &[u8]) -> String {
    String::from_utf8_lossy(b).chars().filter(|c| !c.is_control()).collect()
}

pub fn parse(data: &[u8]) -> Result<Shortcut> {
    let (mut t, mut a) = (vec![0u8; 4096], vec![0u8; 4096]);
    let (mut tl, mut al) = (0usize, 0usize);
    let rc = unsafe { ustan_lnk_parse(data.as_ptr(), data.len(), t.as_mut_ptr(), t.len(), a.as_mut_ptr(), a.len(), &mut tl, &mut al) };
    match rc {
        0 => Ok(Shortcut { target: clean(&t[..tl]), args: clean(&a[..al]).trim().to_string() }),
        -1 => Err(Error::Format("not a .lnk file".into())),
        -2 => Err(Error::Format("shortcut has no local target".into())),
        _ => Err(Error::Format("corrupt shortcut".into())),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn control_characters_are_dropped() {
        assert_eq!(super::clean(b"C:\\App\\a.exe\0"), "C:\\App\\a.exe");
        assert_eq!(super::clean(b"\0\x01-x\n"), "-x");
    }
}
