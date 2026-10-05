//! Safe wrapper over the Zig `ar` parser.
use crate::{Error, Result};

#[repr(C)]
#[derive(Default)]
struct Member {
    offset: usize,
    size: usize,
}

extern "C" {
    fn ustan_ar_find(data: *const u8, len: usize, name: *const u8, name_len: usize, out: *mut Member) -> i32;
}

/// Returns the byte range of member `name` (trailing `*` = prefix match).
pub fn find(data: &[u8], name: &str) -> Result<std::ops::Range<usize>> {
    let mut m = Member::default();
    let rc = unsafe { ustan_ar_find(data.as_ptr(), data.len(), name.as_ptr(), name.len(), &mut m) };
    match rc {
        0 => Ok(m.offset..m.offset + m.size),
        -1 => Err(Error::Format("not an ar archive".into())),
        -2 => Err(Error::Format("truncated ar archive".into())),
        _ => Err(Error::Format(format!("member `{name}` not found"))),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_member() {
        let a = b"!<arch>\ndebian-binary   0           0     0     100644  4         `\n2.0\n";
        let r = super::find(a, "debian-binary").unwrap();
        assert_eq!(&a[r], b"2.0\n");
        assert!(super::find(a, "x").is_err());
    }
}
