const std = @import("std");

pub const ERR_NOT_ELF: i32 = -1;
pub const ERR_UNSUPPORTED: i32 = -2;
pub const ERR_TRUNCATED: i32 = -3;
pub const ERR_NO_SECTION: i32 = -4;

/// End offset of an ELF64 little-endian file as laid out by its headers
/// (end of the section header table). AppImage type 2 appends squashfs there.
pub fn end(data: []const u8, out: *u64) i32 {
    if (data.len < 64 or !std.mem.eql(u8, data[0..4], "\x7fELF")) return ERR_NOT_ELF;
    if (data[4] != 2 or data[5] != 1) return ERR_UNSUPPORTED;
    const shoff = std.mem.readInt(u64, data[0x28..0x30], .little);
    const shentsize = std.mem.readInt(u16, data[0x3A..0x3C], .little);
    const shnum = std.mem.readInt(u16, data[0x3C..0x3E], .little);
    const e = shoff +| @as(u64, shentsize) *| @as(u64, shnum);
    if (e > data.len) return ERR_TRUNCATED;
    out.* = e;
    return 0;
}

/// Locate a named section (e.g. ".upd_info") of an ELF64-LE file. Returns 0 and fills out_off/out_len.
pub fn section(data: []const u8, name: []const u8, out_off: *u64, out_len: *u64) i32 {
    if (data.len < 64 or !std.mem.eql(u8, data[0..4], "\x7fELF")) return ERR_NOT_ELF;
    if (data[4] != 2 or data[5] != 1) return ERR_UNSUPPORTED;
    const shoff = std.mem.readInt(u64, data[0x28..0x30], .little);
    const shentsize: u64 = std.mem.readInt(u16, data[0x3A..0x3C], .little);
    const shnum: u64 = std.mem.readInt(u16, data[0x3C..0x3E], .little);
    const shstrndx: u64 = std.mem.readInt(u16, data[0x3E..0x40], .little);
    if (shentsize < 64 or shstrndx >= shnum) return ERR_TRUNCATED;
    if (shoff +| shentsize *| shnum > data.len) return ERR_TRUNCATED;

    const strhdr = shoff + shstrndx * shentsize;
    const stroff = std.mem.readInt(u64, data[strhdr + 0x18 ..][0..8], .little);
    const strsize = std.mem.readInt(u64, data[strhdr + 0x20 ..][0..8], .little);
    if (stroff +| strsize > data.len) return ERR_TRUNCATED;
    const strtab = data[stroff .. stroff + strsize];

    var i: u64 = 0;
    while (i < shnum) : (i += 1) {
        const h = shoff + i * shentsize;
        const n: usize = std.mem.readInt(u32, data[h..][0..4], .little);
        if (n >= strtab.len) continue;
        const nm = std.mem.sliceTo(strtab[n..], 0);
        if (std.mem.eql(u8, nm, name)) {
            const off = std.mem.readInt(u64, data[h + 0x18 ..][0..8], .little);
            const sz = std.mem.readInt(u64, data[h + 0x20 ..][0..8], .little);
            if (off +| sz > data.len) return ERR_TRUNCATED;
            out_off.* = off;
            out_len.* = sz;
            return 0;
        }
    }
    return ERR_NO_SECTION;
}

test "end of a minimal header" {
    var h = [_]u8{0} ** 128;
    @memcpy(h[0..4], "\x7fELF");
    h[4] = 2;
    h[5] = 1;
    std.mem.writeInt(u64, h[0x28..0x30], 64, .little);
    std.mem.writeInt(u16, h[0x3A..0x3C], 64, .little);
    std.mem.writeInt(u16, h[0x3C..0x3E], 1, .little);
    var o: u64 = 0;
    try std.testing.expectEqual(@as(i32, 0), end(&h, &o));
    try std.testing.expectEqual(@as(u64, 128), o);
    try std.testing.expectEqual(ERR_NOT_ELF, end("nope", &o));
}

test "section lookup finds a named section" {
    var f = [_]u8{0} ** 512;
    @memcpy(f[0..4], "\x7fELF");
    f[4] = 2;
    f[5] = 1;
    // headers at 256: [0]=null, [1]=.shstrtab, [2]=.upd_info
    std.mem.writeInt(u64, f[0x28..0x30], 256, .little);
    std.mem.writeInt(u16, f[0x3A..0x3C], 64, .little);
    std.mem.writeInt(u16, f[0x3C..0x3E], 3, .little);
    std.mem.writeInt(u16, f[0x3E..0x40], 1, .little);
    const names = "\x00.shstrtab\x00.upd_info\x00";
    @memcpy(f[100 .. 100 + names.len], names);
    std.mem.writeInt(u32, f[256 + 64 ..][0..4], 1, .little); // .shstrtab name offset
    std.mem.writeInt(u64, f[256 + 64 + 0x18 ..][0..8], 100, .little);
    std.mem.writeInt(u64, f[256 + 64 + 0x20 ..][0..8], names.len, .little);
    std.mem.writeInt(u32, f[256 + 128 ..][0..4], 11, .little); // ".upd_info"
    std.mem.writeInt(u64, f[256 + 128 + 0x18 ..][0..8], 40, .little);
    std.mem.writeInt(u64, f[256 + 128 + 0x20 ..][0..8], 16, .little);
    var o: u64 = 0;
    var l: u64 = 0;
    try std.testing.expectEqual(@as(i32, 0), section(&f, ".upd_info", &o, &l));
    try std.testing.expectEqual(@as(u64, 40), o);
    try std.testing.expectEqual(@as(u64, 16), l);
    try std.testing.expectEqual(ERR_NO_SECTION, section(&f, ".nope", &o, &l));
}
