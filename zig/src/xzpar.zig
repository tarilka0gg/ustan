//! Parallel .xz reader: an xz file made of several Blocks (xz -T, pixz) is decoded by a pool of
//! threads, and handed out in order with at most `window` Blocks decoded ahead (bounded memory).
const std = @import("std");
const c = @cImport(@cInclude("lzma.h"));

const alloc = std.heap.c_allocator;

const Mutex = struct {
    m: std.c.pthread_mutex_t = std.c.PTHREAD_MUTEX_INITIALIZER,
    fn lock(s: *Mutex) void {
        _ = std.c.pthread_mutex_lock(&s.m);
    }
    fn unlock(s: *Mutex) void {
        _ = std.c.pthread_mutex_unlock(&s.m);
    }
};
const Cond = struct {
    c: std.c.pthread_cond_t = std.c.PTHREAD_COND_INITIALIZER,
    fn wait(s: *Cond, m: *Mutex) void {
        _ = std.c.pthread_cond_wait(&s.c, &m.m);
    }
    fn broadcast(s: *Cond) void {
        _ = std.c.pthread_cond_broadcast(&s.c);
    }
};

const Blk = struct { off: u64, total: u64, unc: u64 };

const Slot = union(enum) { empty, ready: []u8, failed };

pub const Reader = struct {
    fd: c_int,
    check: c.lzma_check,
    blocks: []Blk,
    slots: []Slot,
    threads: []std.Thread,
    mu: Mutex = .{},
    ready_cv: Cond = .{},
    room_cv: Cond = .{},
    next: usize = 0, // next Block a worker will take
    cur: usize = 0, // Block the consumer is reading
    pos: usize = 0, // offset inside that Block
    window: usize,
    stop: bool = false,
};

fn pread_all(fd: c_int, buf: []u8, off: u64) bool {
    var done: usize = 0;
    while (done < buf.len) {
        const n = std.c.pread(fd, buf.ptr + done, buf.len - done, @intCast(off + done));
        if (n <= 0) return false;
        done += @intCast(n);
    }
    return true;
}

fn decodeBlock(r: *Reader, b: Blk) ?[]u8 {
    const inbuf = alloc.alloc(u8, @intCast(b.total)) catch return null;
    defer alloc.free(inbuf);
    if (!pread_all(r.fd, inbuf, b.off)) return null;
    const out = alloc.alloc(u8, @intCast(b.unc)) catch return null;
    errdefer alloc.free(out);

    var filters: [c.LZMA_FILTERS_MAX + 1]c.lzma_filter = undefined;
    var blk = std.mem.zeroes(c.lzma_block);
    blk.version = 0;
    blk.check = r.check;
    blk.filters = &filters;
    blk.header_size = c.lzma_block_header_size_decode(inbuf[0]);
    if (blk.header_size > inbuf.len) {
        alloc.free(out);
        return null;
    }
    if (c.lzma_block_header_decode(&blk, null, inbuf.ptr) != c.LZMA_OK) {
        alloc.free(out);
        return null;
    }
    defer for (&filters) |*f| {
        if (f.id == c.LZMA_VLI_UNKNOWN) break;
        std.c.free(f.options);
    };
    var ip: usize = blk.header_size;
    var op: usize = 0;
    if (c.lzma_block_buffer_decode(&blk, null, inbuf.ptr, &ip, inbuf.len, out.ptr, &op, out.len) != c.LZMA_OK or op != out.len) {
        alloc.free(out);
        return null;
    }
    return out;
}

fn worker(r: *Reader) void {
    while (true) {
        r.mu.lock();
        while (!r.stop and r.next < r.blocks.len and r.next >= r.cur + r.window) r.room_cv.wait(&r.mu);
        if (r.stop or r.next >= r.blocks.len) {
            r.mu.unlock();
            return;
        }
        const i = r.next;
        r.next += 1;
        r.mu.unlock();

        const res = decodeBlock(r, r.blocks[i]);

        r.mu.lock();
        r.slots[i] = if (res) |buf| .{ .ready = buf } else .failed;
        r.ready_cv.broadcast();
        r.mu.unlock();
    }
}

fn openImpl(fd: c_int, nthreads: usize) ?*Reader {
    const size_i = std.c.lseek(fd, 0, std.c.SEEK.END);
    if (size_i < 24) return null;
    const size: u64 = @intCast(size_i);

    var head: [12]u8 = undefined;
    var foot: [12]u8 = undefined;
    if (!pread_all(fd, &head, 0) or !pread_all(fd, &foot, size - 12)) return null;
    var sh: c.lzma_stream_flags = undefined;
    var sf: c.lzma_stream_flags = undefined;
    if (c.lzma_stream_header_decode(&sh, &head) != c.LZMA_OK) return null;
    if (c.lzma_stream_footer_decode(&sf, &foot) != c.LZMA_OK) return null;
    const idx_size: u64 = sf.backward_size;
    if (idx_size + 24 > size) return null;
    const ibuf = alloc.alloc(u8, @intCast(idx_size)) catch return null;
    defer alloc.free(ibuf);
    if (!pread_all(fd, ibuf, size - 12 - idx_size)) return null;

    var memlimit: u64 = std.math.maxInt(u64);
    var idx: ?*c.lzma_index = null;
    var ipos: usize = 0;
    if (c.lzma_index_buffer_decode(&idx, &memlimit, null, ibuf.ptr, &ipos, ibuf.len) != c.LZMA_OK) return null;
    defer c.lzma_index_end(idx, null);
    // one Stream only (no concatenated files, no padding), and worth the threads
    if (c.lzma_index_stream_count(idx) != 1) return null;
    const n: usize = @intCast(c.lzma_index_block_count(idx));
    if (n < 2) return null;

    const blocks = alloc.alloc(Blk, n) catch return null;
    var keep = false;
    defer if (!keep) alloc.free(blocks);
    var it: c.lzma_index_iter = undefined;
    c.lzma_index_iter_init(&it, idx);
    var k: usize = 0;
    while (c.lzma_index_iter_next(&it, c.LZMA_INDEX_ITER_BLOCK) == 0) : (k += 1) {
        if (k >= n) return null;
        blocks[k] = .{ .off = it.block.compressed_file_offset, .total = it.block.total_size, .unc = it.block.uncompressed_size };
    }
    if (k != n) return null;

    const r = alloc.create(Reader) catch return null;
    const slots = alloc.alloc(Slot, n) catch {
        alloc.destroy(r);
        return null;
    };
    @memset(slots, .empty);
    const nt = @max(1, @min(nthreads, n));
    const threads = alloc.alloc(std.Thread, nt) catch {
        alloc.free(slots);
        alloc.destroy(r);
        return null;
    };
    r.* = .{ .fd = fd, .check = sh.check, .blocks = blocks, .slots = slots, .threads = threads, .window = nt };
    var started: usize = 0;
    while (started < nt) : (started += 1) {
        threads[started] = std.Thread.spawn(.{}, worker, .{r}) catch break;
    }
    if (started == 0) {
        alloc.free(threads);
        alloc.free(slots);
        alloc.destroy(r);
        return null;
    }
    r.threads = threads[0..started];
    keep = true;
    return r;
}

pub fn open(path: []const u8, nthreads: usize) ?*Reader {
    const zpath = alloc.dupeZ(u8, path) catch return null;
    defer alloc.free(zpath);
    const fd = std.c.open(zpath, .{ .ACCMODE = .RDONLY }, @as(c_uint, 0));
    if (fd < 0) return null;
    const r = openImpl(fd, nthreads);
    if (r == null) _ = std.c.close(fd);
    return r;
}

/// Copy up to `out.len` decompressed bytes. 0 = end, -1 = a Block failed to decode.
pub fn read(r: *Reader, out: []u8) isize {
    r.mu.lock();
    defer r.mu.unlock();
    while (true) {
        if (r.cur >= r.blocks.len) return 0;
        switch (r.slots[r.cur]) {
            .empty => r.ready_cv.wait(&r.mu),
            .failed => return -1,
            .ready => |buf| {
                const n = @min(out.len, buf.len - r.pos);
                @memcpy(out[0..n], buf[r.pos..][0..n]);
                r.pos += n;
                if (r.pos == buf.len) {
                    alloc.free(buf);
                    r.slots[r.cur] = .empty;
                    r.cur += 1;
                    r.pos = 0;
                    r.room_cv.broadcast();
                }
                return @intCast(n);
            },
        }
    }
}

pub fn close(r: *Reader) void {
    r.mu.lock();
    r.stop = true;
    r.room_cv.broadcast();
    r.mu.unlock();
    for (r.threads) |t| t.join();
    for (r.slots) |s| switch (s) {
        .ready => |buf| alloc.free(buf),
        else => {},
    };
    _ = std.c.close(r.fd);
    alloc.free(r.blocks);
    alloc.free(r.slots);
    alloc.free(r.threads);
    alloc.destroy(r);
}
