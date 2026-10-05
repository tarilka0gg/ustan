const std = @import("std");

pub const ERR_NOT_RPM: i32 = -1;
pub const ERR_TRUNCATED: i32 = -2;
pub const ERR_NO_TAG: i32 = -3;
pub const ERR_BUF_SMALL: i32 = -4;

const rd = std.mem.readInt;
const lead_len = 96;
const hdr_magic = "\x8e\xad\xe8\x01";

const Header = struct {
    nindex: usize,
    store: usize, // file offset of the data store
    end: usize, // file offset right after the header
};

fn header(d: []const u8, at: usize) ?Header {
    if (at + 16 > d.len or !std.mem.eql(u8, d[at..][0..4], hdr_magic)) return null;
    const nindex: usize = rd(u32, d[at + 8 ..][0..4], .big);
    const hsize: usize = rd(u32, d[at + 12 ..][0..4], .big);
    const store = at + 16 + nindex * 16;
    if (nindex > 100_000 or store + hsize > d.len) return null;
    return .{ .nindex = nindex, .store = store, .end = store + hsize };
}

fn mainHeaderOffset(d: []const u8) ?usize {
    if (d.len < lead_len or !std.mem.eql(u8, d[0..4], "\xed\xab\xee\xdb")) return null;
    const sig = header(d, lead_len) orelse return null;
    return (sig.end + 7) & ~@as(usize, 7); // signature header is padded to 8 bytes
}

/// Offset of the compressed cpio payload (right after the main header).
pub fn payload(d: []const u8, out: *u64) i32 {
    if (d.len < lead_len or !std.mem.eql(u8, d[0..4], "\xed\xab\xee\xdb")) return ERR_NOT_RPM;
    const at = mainHeaderOffset(d) orelse return ERR_TRUNCATED;
    const h = header(d, at) orelse return ERR_TRUNCATED;
    out.* = h.end;
    return 0;
}

/// Copy a string-typed tag of the main header into `buf` (no NUL). Returns 0 or an error.
pub fn string(d: []const u8, tag: u32, buf: []u8, out_len: *usize) i32 {
    if (d.len < lead_len or !std.mem.eql(u8, d[0..4], "\xed\xab\xee\xdb")) return ERR_NOT_RPM;
    const at = mainHeaderOffset(d) orelse return ERR_TRUNCATED;
    const h = header(d, at) orelse return ERR_TRUNCATED;
    var i: usize = 0;
    while (i < h.nindex) : (i += 1) {
        const e = d[at + 16 + i * 16 ..][0..16];
        if (rd(u32, e[0..4], .big) != tag) continue;
        const typ = rd(u32, e[4..8], .big);
        if (typ != 6 and typ != 9 and typ != 8) return ERR_NO_TAG; // STRING, I18NSTRING, STRING_ARRAY (first)
        const off: usize = rd(u32, e[8..12], .big);
        if (h.store + off >= h.end) return ERR_TRUNCATED;
        const s = std.mem.sliceTo(d[h.store + off .. h.end], 0);
        if (s.len > buf.len) return ERR_BUF_SMALL;
        @memcpy(buf[0..s.len], s);
        out_len.* = s.len;
        return 0;
    }
    return ERR_NO_TAG;
}

test "parse a tiny hand-made rpm" {
    var f = [_]u8{0} ** 512;
    @memcpy(f[0..4], "\xed\xab\xee\xdb");
    // signature header: 0 entries, 0 bytes -> ends at 112, padded to 112 (already aligned)
    @memcpy(f[96..100], hdr_magic);
    // main header at 112: 1 index entry (NAME, STRING), store "hello\0"
    @memcpy(f[112..116], hdr_magic);
    std.mem.writeInt(u32, f[120..124], 1, .big);
    std.mem.writeInt(u32, f[124..128], 6, .big); // hsize
    std.mem.writeInt(u32, f[128..132], 1000, .big); // tag NAME
    std.mem.writeInt(u32, f[132..136], 6, .big); // STRING
    std.mem.writeInt(u32, f[136..140], 0, .big); // offset
    std.mem.writeInt(u32, f[140..144], 1, .big); // count
    @memcpy(f[144..150], "hello\x00");

    var off: u64 = 0;
    try std.testing.expectEqual(@as(i32, 0), payload(&f, &off));
    try std.testing.expectEqual(@as(u64, 150), off);
    var b: [16]u8 = undefined;
    var n: usize = 0;
    try std.testing.expectEqual(@as(i32, 0), string(&f, 1000, &b, &n));
    try std.testing.expectEqualStrings("hello", b[0..n]);
    try std.testing.expectEqual(ERR_NO_TAG, string(&f, 1001, &b, &n));
    try std.testing.expectEqual(ERR_NOT_RPM, payload("junk", &off));
}
