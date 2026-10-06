//! C-ABI parsers used by the Rust core.
const ar = @import("ar.zig");
const elf = @import("elf.zig");
const pe = @import("pe.zig");
const lnk = @import("lnk.zig");
const rpm = @import("rpm.zig");

pub const Member = ar.Member;

/// Find member `name` (exact match, or prefix match when it ends with '*')
/// in an `ar` archive. Returns 0 on success, negative on error.
export fn ustan_ar_find(
    data: [*]const u8,
    len: usize,
    name: [*]const u8,
    name_len: usize,
    out: *Member,
) i32 {
    return ar.find(data[0..len], name[0..name_len], out);
}

/// End of an ELF64-LE file per its section header table. 0 on success.
export fn ustan_elf_end(data: [*]const u8, len: usize, out: *u64) i32 {
    return elf.end(data[0..len], out);
}

/// Extract the first group icon of a PE file as .ico bytes.
/// Pass out=null to measure. Writes the needed size to *out_len; returns 0 on success.
export fn ustan_pe_icon(data: [*]const u8, len: usize, out: ?[*]u8, cap: usize, out_len: *usize) i32 {
    const buf: ?[]u8 = if (out) |p| p[0..cap] else null;
    return pe.icon(data[0..len], buf, out_len);
}

/// Parse a .lnk shortcut: target path and arguments (UTF-8, not NUL-terminated).
export fn ustan_lnk_parse(
    data: [*]const u8,
    len: usize,
    target: [*]u8,
    target_cap: usize,
    args: [*]u8,
    args_cap: usize,
    target_len: *usize,
    args_len: *usize,
) i32 {
    return lnk.parse(data[0..len], target[0..target_cap], args[0..args_cap], target_len, args_len);
}

/// Offset/length of a named ELF64-LE section. 0 on success.
export fn ustan_elf_section(data: [*]const u8, len: usize, name: [*]const u8, name_len: usize, out_off: *u64, out_len: *u64) i32 {
    return elf.section(data[0..len], name[0..name_len], out_off, out_len);
}

/// Offset of the cpio payload of an RPM. 0 on success.
export fn ustan_rpm_payload(data: [*]const u8, len: usize, out: *u64) i32 {
    return rpm.payload(data[0..len], out);
}

/// String tag (NAME=1000, VERSION=1001, RELEASE=1002, PAYLOADCOMPRESSOR=1125...) of an RPM's main header.
export fn ustan_rpm_string(data: [*]const u8, len: usize, tag: u32, buf: [*]u8, cap: usize, out_len: *usize) i32 {
    return rpm.string(data[0..len], tag, buf[0..cap], out_len);
}

/// DT_NEEDED library names of an ELF64-LE file, newline-separated. 0 on success.
export fn ustan_elf_needed(data: [*]const u8, len: usize, out: [*]u8, cap: usize, out_len: *usize) i32 {
    return elf.needed(data[0..len], out[0..cap], out_len);
}

test {
    _ = rpm;
    _ = lnk;
    _ = pe;
    _ = ar;
    _ = elf;
}
