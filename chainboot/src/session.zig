//! Device side of the protocol: announcing, the handshake and command handling.
//!
//! `Platform` provides the hardware access, which lets the whole state machine
//! run in tests. It must have these methods:
//!   readByte() ?u8                     next received byte, if any
//!   write(bytes: []const u8) void      blocking write
//!   canAnnounce() bool                 true if the magic can be written without blocking
//!   flush() void                       wait until everything written has been sent
//!   nowUs() u64                        monotonic time in microseconds
//!   memWrite(addr: u64, bytes: []const u8) void
//!   memCrc(addr: u64, len: u32) u32    CRC32 of a memory range

const std = @import("std");
const proto = @import("protocol.zig");

/// Half-open memory range [start, end)
pub const Range = struct {
    start: u64,
    end: u64,

    pub fn overlaps(self: Range, other: Range) bool {
        return self.start < other.end and other.start < self.end;
    }

    pub fn contains(self: Range, addr: u64) bool {
        return self.start <= addr and addr < self.end;
    }
};

pub const MemoryRules = struct {
    /// Memory that transfers may write to
    usable: Range,
    /// Ranges inside `usable` that must not be written (the loader, the DTB, ...)
    reserved: []const Range,

    pub fn allows(self: MemoryRules, addr: u64, size: u32) bool {
        const end, const overflow = @addWithOverflow(addr, size);
        if (overflow != 0) {
            return false;
        }

        const range = Range{ .start = addr, .end = end };
        if (range.start < self.usable.start or range.end > self.usable.end) {
            return false;
        }
        for (self.reserved) |reserved| {
            if (range.overlaps(reserved)) {
                return false;
            }
        }
        return true;
    }
};

pub fn Session(comptime Platform: type) type {
    return struct {
        const Self = @This();

        platform: *Platform,
        rules: MemoryRules,

        state: enum { announcing, session } = .announcing,
        last_announce_us: ?u64 = null,
        last_valid_frame_us: u64 = 0,
        matcher: proto.MagicMatcher = .{},
        parser: proto.FrameParser = .{},

        /// The transfer started by the last MEM_WRITE
        transfer: ?Range = null,
        /// Set when END verified the transfer, cleared by any later write
        verified: bool = false,

        pub fn init(platform: *Platform, rules: MemoryRules) Self {
            return .{ .platform = platform, .rules = rules };
        }

        /// Handles everything received so far. Returns the entry point when
        /// the host asked to boot; the `OK` has already been sent by then.
        pub fn poll(self: *Self) ?u64 {
            const now = self.platform.nowUs();

            switch (self.state) {
                .announcing => {
                    const due = if (self.last_announce_us) |last| now -| last >= proto.announce_interval_us else true;
                    if (due and self.platform.canAnnounce()) {
                        self.platform.write(proto.magic);
                        self.last_announce_us = now;
                    }

                    while (self.platform.readByte()) |byte| {
                        if (self.matcher.feed(byte)) {
                            self.startSession(now);
                            break;
                        }
                    }
                },
                .session => {
                    self.parser.checkTimeout(now);
                    while (self.platform.readByte()) |byte| {
                        const event = self.parser.feed(byte, now) orelse continue;
                        switch (event) {
                            .frame => |frame| {
                                self.last_valid_frame_us = now;
                                if (self.handleFrame(frame)) |entry| {
                                    return entry;
                                }
                            },
                            .bad_crc => |msg_type| _ = self.sendErr(msg_type, .bad_frame),
                        }
                    }

                    if (now -| self.last_valid_frame_us > proto.idle_timeout_us) {
                        self.state = .announcing;
                        self.last_announce_us = null;
                        self.matcher.reset();
                        self.clearTransfer();
                    }
                },
            }
            return null;
        }

        fn startSession(self: *Self, now: u64) void {
            self.state = .session;
            self.last_valid_frame_us = now;
            self.parser.reset();
            self.clearTransfer();
            self.sendInfo();
        }

        fn clearTransfer(self: *Self) void {
            self.transfer = null;
            self.verified = false;
        }

        fn handleFrame(self: *Self, frame: proto.Frame) ?u64 {
            const p = frame.payload;
            switch (frame.msg_type) {
                .ping => {
                    if (p.len != 0) return self.sendErr(frame.msg_type, .bad_payload);
                    self.sendOk(.ping, &.{});
                },
                .info => {
                    if (p.len != 0) return self.sendErr(frame.msg_type, .bad_payload);
                    self.clearTransfer();
                    self.sendInfo();
                },
                .mem_write => {
                    if (p.len != 12) return self.sendErr(frame.msg_type, .bad_payload);
                    const addr = std.mem.readInt(u64, p[0..8], .little);
                    const size = std.mem.readInt(u32, p[8..12], .little);
                    if (size == 0) return self.sendErr(frame.msg_type, .bad_payload);
                    if (!self.rules.allows(addr, size)) return self.sendErr(frame.msg_type, .bad_address);

                    self.transfer = .{ .start = addr, .end = addr + size };
                    self.verified = false;
                    self.sendOk(.mem_write, &.{});
                },
                .data => {
                    if (p.len < 4) return self.sendErr(frame.msg_type, .bad_payload);
                    const transfer = self.transfer orelse return self.sendErr(frame.msg_type, .no_transfer);
                    const offset = std.mem.readInt(u32, p[0..4], .little);
                    const bytes = p[4..];
                    if (@as(u64, offset) + bytes.len > transfer.end - transfer.start) {
                        return self.sendErr(frame.msg_type, .out_of_range);
                    }

                    self.platform.memWrite(transfer.start + offset, bytes);
                    self.verified = false;
                    self.sendOk(.data, &.{});
                },
                .end => {
                    if (p.len != 4) return self.sendErr(frame.msg_type, .bad_payload);
                    const transfer = self.transfer orelse return self.sendErr(frame.msg_type, .no_transfer);
                    const expected = std.mem.readInt(u32, p[0..4], .little);
                    const actual = self.platform.memCrc(transfer.start, @intCast(transfer.end - transfer.start));
                    if (actual != expected) return self.sendErr(frame.msg_type, .checksum);

                    self.verified = true;
                    self.sendOk(.end, &.{});
                },
                .boot => {
                    if (p.len != 8) return self.sendErr(frame.msg_type, .bad_payload);
                    const entry = std.mem.readInt(u64, p[0..8], .little);
                    const transfer = self.transfer orelse return self.sendErr(frame.msg_type, .not_verified);
                    if (!self.verified or !transfer.contains(entry)) {
                        return self.sendErr(frame.msg_type, .not_verified);
                    }

                    self.sendOk(.boot, &.{});
                    self.platform.flush();
                    return entry;
                },
                else => return self.sendErr(frame.msg_type, .unsupported),
            }
            return null;
        }

        fn sendInfo(self: *Self) void {
            var info: [8]u8 = undefined;
            std.mem.writeInt(u16, info[0..2], proto.version, .little);
            std.mem.writeInt(u16, info[2..4], proto.max_payload, .little);
            std.mem.writeInt(u32, info[4..8], @bitCast(proto.Capabilities{ .mem_boot = true }), .little);
            self.sendOk(.info, &info);
        }

        fn sendOk(self: *Self, req_type: proto.MsgType, data: []const u8) void {
            var buf: [32]u8 = undefined;
            self.platform.write(proto.encodeFrame(&buf, .ok, &.{ &.{@intFromEnum(req_type)}, data }));
        }

        /// Returns null so handlers can `return self.sendErr(...)`
        fn sendErr(self: *Self, req_type: proto.MsgType, code: proto.ErrorCode) ?u64 {
            var buf: [16]u8 = undefined;
            self.platform.write(proto.encodeFrame(&buf, .err, &.{&.{ @intFromEnum(req_type), @intFromEnum(code) }}));
            return null;
        }
    };
}

const testing = std.testing;

const FakePlatform = struct {
    const mem_base = 0x80000;

    rx: std.ArrayList(u8) = .empty,
    rx_pos: usize = 0,
    tx: std.ArrayList(u8) = .empty,
    now: u64 = 0,
    mem: [0x10000]u8 = @splat(0),
    flushed: bool = false,

    fn deinit(self: *FakePlatform) void {
        self.rx.deinit(testing.allocator);
        self.tx.deinit(testing.allocator);
    }

    pub fn readByte(self: *FakePlatform) ?u8 {
        if (self.rx_pos == self.rx.items.len) return null;
        defer self.rx_pos += 1;
        return self.rx.items[self.rx_pos];
    }

    pub fn write(self: *FakePlatform, bytes: []const u8) void {
        self.tx.appendSlice(testing.allocator, bytes) catch @panic("OOM");
    }

    pub fn canAnnounce(_: *FakePlatform) bool {
        return true;
    }

    pub fn flush(self: *FakePlatform) void {
        self.flushed = true;
    }

    pub fn nowUs(self: *FakePlatform) u64 {
        return self.now;
    }

    pub fn memWrite(self: *FakePlatform, addr: u64, bytes: []const u8) void {
        @memcpy(self.mem[addr - mem_base ..][0..bytes.len], bytes);
    }

    pub fn memCrc(self: *FakePlatform, addr: u64, len: u32) u32 {
        return std.hash.Crc32.hash(self.mem[addr - mem_base ..][0..len]);
    }
};

const Response = union(enum) {
    ok: struct { req: proto.MsgType, data: []const u8 },
    err: struct { req: proto.MsgType, code: proto.ErrorCode },
};

const Harness = struct {
    platform: FakePlatform = .{},
    session: Session(FakePlatform) = undefined,
    parser: proto.FrameParser = .{},
    tx_pos: usize = 0,

    const reserved = [_]Range{.{ .start = 0x88000, .end = 0x89000 }};
    const rules = MemoryRules{
        .usable = .{ .start = 0x80000, .end = 0x90000 },
        .reserved = &reserved,
    };

    fn init(self: *Harness) void {
        self.session = .init(&self.platform, rules);
    }

    fn deinit(self: *Harness) void {
        self.platform.deinit();
    }

    fn sendRaw(self: *Harness, bytes: []const u8) void {
        self.platform.rx.appendSlice(testing.allocator, bytes) catch @panic("OOM");
    }

    fn send(self: *Harness, msg_type: proto.MsgType, parts: []const []const u8) void {
        var buf: [proto.max_frame_len]u8 = undefined;
        self.sendRaw(proto.encodeFrame(&buf, msg_type, parts));
    }

    fn poll(self: *Harness) ?u64 {
        return self.session.poll();
    }

    /// Parses the next frame the device sent
    fn response(self: *Harness) ?Response {
        while (self.tx_pos < self.platform.tx.items.len) {
            const byte = self.platform.tx.items[self.tx_pos];
            self.tx_pos += 1;
            const event = self.parser.feed(byte, 0) orelse continue;
            const frame = event.frame;
            return switch (frame.msg_type) {
                .ok => .{ .ok = .{ .req = @enumFromInt(frame.payload[0]), .data = frame.payload[1..] } },
                .err => .{ .err = .{ .req = @enumFromInt(frame.payload[0]), .code = @enumFromInt(frame.payload[1]) } },
                else => unreachable,
            };
        }
        return null;
    }

    fn expectOk(self: *Harness, req: proto.MsgType) !void {
        const r = self.response() orelse return error.NoResponse;
        try testing.expectEqual(req, r.ok.req);
    }

    fn expectErr(self: *Harness, req: proto.MsgType, code: proto.ErrorCode) !void {
        const r = self.response() orelse return error.NoResponse;
        try testing.expectEqual(req, r.err.req);
        try testing.expectEqual(code, r.err.code);
    }

    fn handshake(self: *Harness) !void {
        _ = self.poll();
        self.sendRaw(proto.magic);
        _ = self.poll();
        // Skip the announce sequence in front of the response
        self.tx_pos = self.platform.tx.items.len - (proto.header_len + 9 + proto.crc_len);
        try self.expectOk(.info);
    }

    fn memWrite(self: *Harness, addr: u64, size: u32) void {
        var p: [12]u8 = undefined;
        std.mem.writeInt(u64, p[0..8], addr, .little);
        std.mem.writeInt(u32, p[8..12], size, .little);
        self.send(.mem_write, &.{&p});
        _ = self.poll();
    }

    fn data(self: *Harness, offset: u32, bytes: []const u8) void {
        var p: [4]u8 = undefined;
        std.mem.writeInt(u32, &p, offset, .little);
        self.send(.data, &.{ &p, bytes });
        _ = self.poll();
    }

    fn end(self: *Harness, crc: u32) void {
        var p: [4]u8 = undefined;
        std.mem.writeInt(u32, &p, crc, .little);
        self.send(.end, &.{&p});
        _ = self.poll();
    }

    fn boot(self: *Harness, entry: u64) ?u64 {
        var p: [8]u8 = undefined;
        std.mem.writeInt(u64, &p, entry, .little);
        self.send(.boot, &.{&p});
        return self.poll();
    }
};

test "announces the magic sequence every interval" {
    var h = Harness{};
    h.init();
    defer h.deinit();

    _ = h.poll();
    try testing.expectEqualStrings(proto.magic, h.platform.tx.items);
    h.platform.now = proto.announce_interval_us - 1;
    _ = h.poll();
    try testing.expectEqual(proto.magic.len, h.platform.tx.items.len);
    h.platform.now = proto.announce_interval_us;
    _ = h.poll();
    try testing.expectEqual(2 * proto.magic.len, h.platform.tx.items.len);
}

test "handshake replies with INFO" {
    var h = Harness{};
    h.init();
    defer h.deinit();

    _ = h.poll();
    h.sendRaw("noise" ++ proto.magic);
    _ = h.poll();
    h.tx_pos = proto.magic.len;
    const r = h.response().?;
    try testing.expectEqual(proto.MsgType.info, r.ok.req);
    try testing.expectEqualSlices(u8, &.{ 0x01, 0x00, 0x00, 0x10, 0x01, 0x00, 0x00, 0x00 }, r.ok.data);

    // No more announcing once in a session
    const len = h.platform.tx.items.len;
    h.platform.now = 10 * proto.announce_interval_us;
    _ = h.poll();
    try testing.expectEqual(len, h.platform.tx.items.len);
}

test "loads an image and boots it" {
    var h = Harness{};
    h.init();
    defer h.deinit();
    try h.handshake();

    const image = "a kernel image";
    h.memWrite(0x80000, image.len);
    try h.expectOk(.mem_write);
    h.data(0, image[0..5]);
    try h.expectOk(.data);
    h.data(5, image[5..]);
    try h.expectOk(.data);
    // Chunks can be repeated
    h.data(0, image[0..5]);
    try h.expectOk(.data);
    h.end(std.hash.Crc32.hash(image));
    try h.expectOk(.end);

    try testing.expectEqual(0x80000, h.boot(0x80000));
    try h.expectOk(.boot);
    try testing.expect(h.platform.flushed);
    try testing.expectEqualStrings(image, h.platform.mem[0..image.len]);
}

test "magic sequence in a session is treated as data" {
    var h = Harness{};
    h.init();
    defer h.deinit();
    try h.handshake();

    h.memWrite(0x80000, proto.magic.len);
    try h.expectOk(.mem_write);
    h.sendRaw(proto.magic);
    h.data(0, proto.magic);
    try h.expectOk(.data);
    h.end(std.hash.Crc32.hash(proto.magic));
    try h.expectOk(.end);
}

test "boot requires a verified transfer containing the entry point" {
    var h = Harness{};
    h.init();
    defer h.deinit();
    try h.handshake();

    try testing.expectEqual(null, h.boot(0x80000));
    try h.expectErr(.boot, .not_verified);

    h.memWrite(0x80000, 4);
    try h.expectOk(.mem_write);
    h.data(0, "abcd");
    try h.expectOk(.data);
    try testing.expectEqual(null, h.boot(0x80000));
    try h.expectErr(.boot, .not_verified);

    h.end(std.hash.Crc32.hash("abcd"));
    try h.expectOk(.end);
    try testing.expectEqual(null, h.boot(0x80004));
    try h.expectErr(.boot, .not_verified);

    // Writing after END invalidates the verification
    h.data(0, "abcd");
    try h.expectOk(.data);
    try testing.expectEqual(null, h.boot(0x80000));
    try h.expectErr(.boot, .not_verified);
}

test "end detects a checksum mismatch" {
    var h = Harness{};
    h.init();
    defer h.deinit();
    try h.handshake();

    h.memWrite(0x80000, 4);
    try h.expectOk(.mem_write);
    h.data(0, "abcd");
    try h.expectOk(.data);
    h.end(std.hash.Crc32.hash("abce"));
    try h.expectErr(.end, .checksum);
}

test "rejects disallowed memory ranges" {
    var h = Harness{};
    h.init();
    defer h.deinit();
    try h.handshake();

    h.memWrite(0x7FFFF, 2);
    try h.expectErr(.mem_write, .bad_address);
    h.memWrite(0x87FFF, 2);
    try h.expectErr(.mem_write, .bad_address);
    h.memWrite(0x8FFFF, 2);
    try h.expectErr(.mem_write, .bad_address);
    h.memWrite(std.math.maxInt(u64), 2);
    try h.expectErr(.mem_write, .bad_address);
    h.memWrite(0x80000, 0);
    try h.expectErr(.mem_write, .bad_payload);
    h.memWrite(0x87000, 0x1000);
    try h.expectOk(.mem_write);
}

test "data needs an active transfer and must fit in it" {
    var h = Harness{};
    h.init();
    defer h.deinit();
    try h.handshake();

    h.data(0, "abcd");
    try h.expectErr(.data, .no_transfer);
    h.memWrite(0x80000, 4);
    try h.expectOk(.mem_write);
    h.data(1, "abcd");
    try h.expectErr(.data, .out_of_range);
}

test "INFO in a session clears the transfer" {
    var h = Harness{};
    h.init();
    defer h.deinit();
    try h.handshake();

    h.memWrite(0x80000, 4);
    try h.expectOk(.mem_write);
    h.send(.info, &.{});
    _ = h.poll();
    try h.expectOk(.info);
    h.data(0, "abcd");
    try h.expectErr(.data, .no_transfer);
}

test "reports bad frames, unknown types and bad payloads" {
    var h = Harness{};
    h.init();
    defer h.deinit();
    try h.handshake();

    var buf: [proto.max_frame_len]u8 = undefined;
    const frame = proto.encodeFrame(&buf, .ping, &.{});
    frame[frame.len - 1] ^= 0xFF;
    h.sendRaw(frame);
    _ = h.poll();
    try h.expectErr(.ping, .bad_frame);

    h.send(@enumFromInt(0x42), &.{});
    _ = h.poll();
    try h.expectErr(@enumFromInt(0x42), .unsupported);

    h.send(.ping, &.{"x"});
    _ = h.poll();
    try h.expectErr(.ping, .bad_payload);
}

test "goes back to announcing after the idle timeout" {
    var h = Harness{};
    h.init();
    defer h.deinit();
    try h.handshake();

    h.platform.now = proto.idle_timeout_us / 2;
    h.send(.ping, &.{});
    _ = h.poll();
    try h.expectOk(.ping);

    h.platform.now += proto.idle_timeout_us + 1;
    _ = h.poll();
    const len = h.platform.tx.items.len;
    _ = h.poll();
    try testing.expectEqualStrings(proto.magic, h.platform.tx.items[len..]);
}
