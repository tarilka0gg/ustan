use crate::{Error, Result};

extern "C" {
    fn ustan_pe_icon(data: *const u8, len: usize, out: *mut u8, cap: usize, out_len: *mut usize) -> i32;
}

fn call(data: &[u8], out: Option<&mut [u8]>) -> (i32, usize) {
    let mut n = 0usize;
    let (p, cap) = out.map_or((std::ptr::null_mut(), 0), |o| (o.as_mut_ptr(), o.len()));
    let rc = unsafe { ustan_pe_icon(data.as_ptr(), data.len(), p, cap, &mut n) };
    (rc, n)
}

/// The executable's first icon group as `.ico` bytes (`None` if it has no icon).
pub fn icon_ico(data: &[u8]) -> Result<Option<Vec<u8>>> {
    let (rc, n) = call(data, None);
    match rc {
        0 => {}
        -2 => return Ok(None),
        -1 => return Err(Error::Format("not a PE executable".into())),
        _ => return Err(Error::Format("corrupt PE resources".into())),
    }
    let mut buf = vec![0u8; n];
    match call(data, Some(&mut buf)) {
        (0, m) if m == n => Ok(Some(buf)),
        _ => Err(Error::Format("corrupt PE resources".into())),
    }
}

/// The best (largest) icon rendered as PNG bytes.
pub fn icon_png(data: &[u8]) -> Result<Option<Vec<u8>>> {
    let Some(ico) = icon_ico(data)? else { return Ok(None) };
    let img = image::load_from_memory_with_format(&ico, image::ImageFormat::Ico)
        .map_err(|e| Error::Format(format!("icon decode: {e}")))?;
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| Error::Format(format!("icon encode: {e}")))?;
    Ok(Some(png))
}
