const std = @import("std");

pub const Member = extern struct {
    offset: usize,
    size: usize,
};

const magic = "!<arch>\n";
const hdr_len = 60;

pub const ERR_BAD_MAGIC: i32 = -1;
pub const ERR_TRUNCATED: i32 = -2;
pub const ERR_NOT_FOUND: i32 = -3;

pub fn find(data: []const u8, want: []const u8, out: *Member) i32 {
    if (data.len < magic.len or !std.mem.eql(u8, data[0..magic.len], magic))
        return ERR_BAD_MAGIC;

    const prefix = want.len > 0 and want[want.len - 1] == '*';
    const pat = if (prefix) want[0 .. want.len - 1] else want;

    var pos: usize = magic.len;
    while (pos + hdr_len <= data.len) {
        const hdr = data[pos .. pos + hdr_len];
        if (hdr[58] != '`' or hdr[59] != '\n') return ERR_TRUNCATED;

        const name = std.mem.trimEnd(u8, hdr[0..16], " ");
        const clean = std.mem.trimEnd(u8, name, "/");
        const size = std.fmt.parseInt(usize, std.mem.trim(u8, hdr[48..58], " "), 10) catch
            return ERR_TRUNCATED;

        const body = pos + hdr_len;
        if (body + size > data.len) return ERR_TRUNCATED;

        const hit = if (prefix) std.mem.startsWith(u8, clean, pat) else std.mem.eql(u8, clean, pat);
        if (hit) {
            out.* = .{ .offset = body, .size = size };
            return 0;
        }
        pos = body + size + (size & 1);
    }
    return ERR_NOT_FOUND;
}

test "find members in a tiny archive" {
    const a = "!<arch>\n" ++
        "debian-binary   0           0     0     100644  4         `\n2.0\n" ++
        "data.tar.xz     0           0     0     100644  3         `\nabc\n";
    var m: Member = undefined;
    try std.testing.expectEqual(@as(i32, 0), find(a, "data.tar*", &m));
    try std.testing.expectEqualStrings("abc", a[m.offset .. m.offset + m.size]);
    try std.testing.expectEqual(ERR_NOT_FOUND, find(a, "nope", &m));
    try std.testing.expectEqual(ERR_BAD_MAGIC, find("garbage!", "x", &m));
}
