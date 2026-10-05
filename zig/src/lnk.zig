const std = @import("std");

pub const ERR_NOT_LNK: i32 = -1;
pub const ERR_NO_TARGET: i32 = -2;
pub const ERR_TRUNCATED: i32 = -3;
pub const ERR_BUF_SMALL: i32 = -4;

const rd = std.mem.readInt;

const Out = struct {
    buf: []u8,
    len: usize = 0,
    fn byte(self: *Out, b: u8) bool {
        if (self.len >= self.buf.len) return false;
        self.buf[self.len] = b;
        self.len += 1;
        return true;
    }
    fn cp(self: *Out, c: u21) bool {
        var tmp: [4]u8 = undefined;
        const n = std.unicode.utf8Encode(c, &tmp) catch return true; // drop invalid
        for (tmp[0..n]) |b| if (!self.byte(b)) return false;
        return true;
    }
};

/// Append `n` UTF-16LE code units (or 8-bit chars) read from `d[at..]`.
fn text(out: *Out, d: []const u8, at: usize, n: usize, wide: bool) bool {
    if (wide) {
        var i: usize = 0;
        while (i < n) : (i += 1) {
            if (at + i * 2 + 2 > d.len) return false;
            var u: u21 = rd(u16, d[at + i * 2 ..][0..2], .little);
            if (u >= 0xD800 and u < 0xDC00 and i + 1 < n and at + i * 2 + 4 <= d.len) {
                const lo = rd(u16, d[at + i * 2 + 2 ..][0..2], .little);
                if (lo >= 0xDC00 and lo < 0xE000) {
                    u = 0x10000 + ((u - 0xD800) << 10) + (lo - 0xDC00);
                    i += 1;
                }
            }
            if (!out.cp(u)) return false;
        }
    } else {
        if (at + n > d.len) return false;
        for (d[at .. at + n]) |b| if (!out.byte(b)) return false;
    }
    return true;
}

fn cstr(d: []const u8, at: usize, wide: bool) ?usize {
    var i = at;
    if (wide) {
        while (i + 2 <= d.len) : (i += 2) if (d[i] == 0 and d[i + 1] == 0) return (i - at) / 2;
    } else {
        while (i < d.len) : (i += 1) if (d[i] == 0) return i - at;
    }
    return null;
}

/// Read the target path and command-line arguments of a Windows .lnk shortcut.
pub fn parse(d: []const u8, target: []u8, args: []u8, tlen: *usize, alen: *usize) i32 {
    if (d.len < 0x4C or rd(u32, d[0..4], .little) != 0x4C) return ERR_NOT_LNK;
    const flags = rd(u32, d[0x14..0x18], .little);
    const unicode = flags & 0x80 != 0;
    var pos: usize = 0x4C;
    if (flags & 1 != 0) {
        if (pos + 2 > d.len) return ERR_TRUNCATED;
        pos += 2 + rd(u16, d[pos..][0..2], .little);
    }
    var t = Out{ .buf = target };
    var found = false;
    if (flags & 2 != 0) {
        if (pos + 28 > d.len) return ERR_TRUNCATED;
        const li = d[pos..];
        const size: usize = rd(u32, li[0..4], .little);
        if (size < 28 or pos + size > d.len) return ERR_TRUNCATED;
        const hdr = rd(u32, li[4..8], .little);
        const lf = rd(u32, li[8..12], .little);
        if (lf & 1 != 0) {
            const wide = hdr >= 0x24;
            const base: usize = rd(u32, li[if (wide) 28 else 16 ..][0..4], .little);
            const suf: usize = rd(u32, li[if (wide) 32 else 24 ..][0..4], .little);
            const bn = cstr(li[0..size], base, wide) orelse return ERR_TRUNCATED;
            if (!text(&t, li[0..size], base, bn, wide)) return ERR_BUF_SMALL;
            if (suf != 0) {
                const sn = cstr(li[0..size], suf, wide) orelse return ERR_TRUNCATED;
                if (!text(&t, li[0..size], suf, sn, wide)) return ERR_BUF_SMALL;
            }
            found = true;
        }
        pos += size;
    }
    if (!found) return ERR_NO_TARGET;
    tlen.* = t.len;

    // StringData: NAME, RELATIVE_PATH, WORKING_DIR, ARGUMENTS, ICON_LOCATION
    var a = Out{ .buf = args };
    var bit: u5 = 2;
    while (bit <= 6) : (bit += 1) {
        if (flags & (@as(u32, 1) << bit) == 0) continue;
        if (pos + 2 > d.len) break;
        const n: usize = rd(u16, d[pos..][0..2], .little);
        pos += 2;
        const bytes = if (unicode) n * 2 else n;
        if (bit == 5 and !text(&a, d, pos, n, unicode)) return ERR_BUF_SMALL;
        pos += bytes;
    }
    alen.* = a.len;
    return 0;
}

test "rejects garbage" {
    var t: [8]u8 = undefined;
    var a: [8]u8 = undefined;
    var tl: usize = 0;
    var al: usize = 0;
    try std.testing.expectEqual(ERR_NOT_LNK, parse("nope", &t, &a, &tl, &al));
}

test "ascii local path with args" {
    var f = [_]u8{0} ** 0x200;
    std.mem.writeInt(u32, f[0..4], 0x4C, .little);
    std.mem.writeInt(u32, f[0x14..0x18], 0x2 | 0x20 | 0x80, .little); // LinkInfo + args + unicode
    const li: usize = 0x4C;
    const path = "C:\\App\\a.exe\x00";
    std.mem.writeInt(u32, f[li..][0..4], 28 + path.len + 1, .little);
    std.mem.writeInt(u32, f[li + 4 ..][0..4], 28, .little);
    std.mem.writeInt(u32, f[li + 8 ..][0..4], 1, .little);
    std.mem.writeInt(u32, f[li + 16 ..][0..4], 28, .little);
    @memcpy(f[li + 28 ..][0..path.len], path);
    // suffix offset points at an empty string
    std.mem.writeInt(u32, f[li + 24 ..][0..4], @intCast(28 + path.len), .little);
    const sd = li + 28 + path.len + 1;
    std.mem.writeInt(u16, f[sd..][0..2], 2, .little);
    f[sd + 2] = '-';
    f[sd + 4] = 'x';
    var t: [64]u8 = undefined;
    var a: [64]u8 = undefined;
    var tl: usize = 0;
    var al: usize = 0;
    try std.testing.expectEqual(@as(i32, 0), parse(f[0 .. sd + 6], &t, &a, &tl, &al));
    try std.testing.expectEqualStrings("C:\\App\\a.exe", t[0..tl]);
    try std.testing.expectEqualStrings("-x", a[0..al]);
}
