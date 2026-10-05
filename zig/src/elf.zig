const std = @import("std");

pub const ERR_NOT_ELF: i32 = -1;
pub const ERR_UNSUPPORTED: i32 = -2;
pub const ERR_TRUNCATED: i32 = -3;

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
