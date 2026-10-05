const std = @import("std");

pub const ERR_NOT_PE: i32 = -1;
pub const ERR_NO_ICON: i32 = -2;
pub const ERR_TRUNCATED: i32 = -3;
pub const ERR_BUF_SMALL: i32 = -4;

const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;

const Section = struct { va: u32, vsize: u32, raw: u32, rawsize: u32 };

const Pe = struct {
    d: []const u8,
    secs: [96]Section = undefined,
    nsecs: usize = 0,
    res_off: usize = 0, // file offset of resource root
    res_rva: u32 = 0, // 0 = no resource directory

    fn u16at(self: *const Pe, o: usize) ?u16 {
        if (o + 2 > self.d.len) return null;
        return std.mem.readInt(u16, self.d[o..][0..2], .little);
    }
    fn u32at(self: *const Pe, o: usize) ?u32 {
        if (o + 4 > self.d.len) return null;
        return std.mem.readInt(u32, self.d[o..][0..4], .little);
    }

    fn rvaToOff(self: *const Pe, rva: u32) ?usize {
        for (self.secs[0..self.nsecs]) |s| {
            const span = @max(s.vsize, s.rawsize);
            if (rva >= s.va and rva - s.va < span) return @as(usize, s.raw) + (rva - s.va);
        }
        return null;
    }

    /// Entry of resource directory at `dir` (relative to root) with integer id; returns raw offset field.
    fn entry(self: *const Pe, dir: u32, id: ?u32) ?u32 {
        const base = self.res_off + dir;
        const named = self.u16at(base + 12) orelse return null;
        const ids = self.u16at(base + 14) orelse return null;
        var i: usize = named;
        while (i < @as(usize, named) + ids) : (i += 1) {
            const e = base + 16 + i * 8;
            const name = self.u32at(e) orelse return null;
            const off = self.u32at(e + 4) orelse return null;
            if (id == null or name == id.?) return off;
        }
        return null;
    }

    /// Follow type -> name -> language and return the (data offset, size) of the leaf.
    fn leaf(self: *const Pe, rtype: u32, id: ?u32) ?struct { off: usize, size: usize } {
        const t = self.entry(0, rtype) orelse return null;
        if (t & 0x80000000 == 0) return null;
        const n = self.entry(t & 0x7fffffff, id) orelse return null;
        if (n & 0x80000000 == 0) return null;
        const l = self.entry(n & 0x7fffffff, null) orelse return null;
        if (l & 0x80000000 != 0) return null;
        const de = self.res_off + l;
        const rva = self.u32at(de) orelse return null;
        const size = self.u32at(de + 4) orelse return null;
        const off = self.rvaToOff(rva) orelse return null;
        if (off + size > self.d.len) return null;
        return .{ .off = off, .size = size };
    }
};

const Sink = struct {
    buf: ?[]u8,
    pos: usize = 0,
    fn put(self: *Sink, b: []const u8) void {
        if (self.buf) |buf| if (self.pos + b.len <= buf.len) @memcpy(buf[self.pos..][0..b.len], b);
        self.pos += b.len;
    }
    fn int(self: *Sink, comptime T: type, v: T) void {
        var tmp: [@sizeOf(T)]u8 = undefined;
        std.mem.writeInt(T, &tmp, v, .little);
        self.put(&tmp);
    }
};

fn parse(d: []const u8) ?Pe {
    var pe = Pe{ .d = d };
    if (d.len < 0x40 or d[0] != 'M' or d[1] != 'Z') return null;
    const lfanew: usize = pe.u32at(0x3C) orelse return null;
    if (lfanew + 24 > d.len or !std.mem.eql(u8, d[lfanew..][0..4], "PE\x00\x00")) return null;
    const nsec: usize = pe.u16at(lfanew + 6) orelse return null;
    const optsize: usize = pe.u16at(lfanew + 20) orelse return null;
    const opt = lfanew + 24;
    const magic = pe.u16at(opt) orelse return null;
    const dd: usize = if (magic == 0x20b) opt + 112 else if (magic == 0x10b) opt + 96 else return null;
    const rsrc_rva = pe.u32at(dd + 16) orelse return null;
    if (nsec > pe.secs.len) return null;
    const st = opt + optsize;
    var i: usize = 0;
    while (i < nsec) : (i += 1) {
        const s = st + i * 40;
        pe.secs[i] = .{
            .vsize = pe.u32at(s + 8) orelse return null,
            .va = pe.u32at(s + 12) orelse return null,
            .rawsize = pe.u32at(s + 16) orelse return null,
            .raw = pe.u32at(s + 20) orelse return null,
        };
    }
    pe.nsecs = nsec;
    pe.res_rva = rsrc_rva;
    if (rsrc_rva != 0) pe.res_off = pe.rvaToOff(rsrc_rva) orelse return null;
    return pe;
}

/// Build an .ico from the first group icon of the executable. `out` may be null to measure.
pub fn icon(d: []const u8, out: ?[]u8, out_len: *usize) i32 {
    const pe = parse(d) orelse return ERR_NOT_PE;
    if (pe.res_rva == 0) return ERR_NO_ICON;
    const grp = pe.leaf(RT_GROUP_ICON, null) orelse return ERR_NO_ICON;
    if (grp.size < 6) return ERR_TRUNCATED;
    const g = d[grp.off..][0..grp.size];
    const count: usize = std.mem.readInt(u16, g[4..6], .little);
    if (count == 0 or 6 + count * 14 > g.len) return ERR_TRUNCATED;

    var sink = Sink{ .buf = out };
    sink.int(u16, 0);
    sink.int(u16, 1);
    sink.int(u16, @intCast(count));
    var data_off: usize = 6 + count * 16;
    var i: usize = 0;
    while (i < count) : (i += 1) {
        const e = g[6 + i * 14 ..][0..14];
        const nid = std.mem.readInt(u16, e[12..14], .little);
        const leaf = pe.leaf(RT_ICON, nid) orelse return ERR_TRUNCATED;
        sink.put(e[0..4]); // width, height, colors, reserved
        sink.put(e[4..8]); // planes, bitcount
        sink.int(u32, @intCast(leaf.size));
        sink.int(u32, @intCast(data_off));
        data_off += leaf.size;
    }
    i = 0;
    while (i < count) : (i += 1) {
        const nid = std.mem.readInt(u16, g[6 + i * 14 + 12 ..][0..2], .little);
        const leaf = pe.leaf(RT_ICON, nid) orelse return ERR_TRUNCATED;
        sink.put(d[leaf.off..][0..leaf.size]);
    }
    out_len.* = sink.pos;
    if (out) |b| if (sink.pos > b.len) return ERR_BUF_SMALL;
    return 0;
}

test "rejects non-PE; PE without resources has no icon" {
    var n: usize = 0;
    try std.testing.expectEqual(ERR_NOT_PE, icon("hello", null, &n));
    var h = [_]u8{0} ** 0x200;
    h[0] = 'M';
    h[1] = 'Z';
    std.mem.writeInt(u32, h[0x3C..0x40], 0x80, .little);
    @memcpy(h[0x80..0x84], "PE\x00\x00");
    std.mem.writeInt(u16, h[0x80 + 20 ..][0..2], 0xF0, .little);
    std.mem.writeInt(u16, h[0x98..][0..2], 0x20b, .little);
    try std.testing.expectEqual(ERR_NO_ICON, icon(&h, null, &n));
}
