//! Wire format of the protocol described in docs/protocol.md.
//! Nothing in here touches hardware, so it can be tested on the host.

const std = @import("std");

pub const version: u16 = 1;

pub const magic = "PICHBT\r\n";
pub const sync = [2]u8{ 0xA5, 0x5A };

/// Largest frame payload the device accepts
pub const max_payload: u16 = 4096;
/// sync + type + len
pub const header_len = 5;
pub const crc_len = 4;
pub const max_frame_len = header_len + max_payload + crc_len;

pub const announce_interval_us = 200_000;
pub const frame_timeout_us = 500_000;
pub const idle_timeout_us = 10_000_000;

pub const Capabilities = packed struct(u32) {
    mem_boot: bool = false,
    sd_files: bool = false,
    _reserved: u30 = 0,
};

pub const MsgType = enum(u8) {
    ping = 0x01,
    info = 0x02,
    mem_write = 0x10,
    data = 0x11,
    end = 0x12,
    boot = 0x13,
    ok = 0x80,
    err = 0xFF,
    _,
};

pub const ErrorCode = enum(u8) {
    bad_frame = 0x01,
    unsupported = 0x02,
    bad_payload = 0x03,
    bad_address = 0x04,
    no_transfer = 0x05,
    out_of_range = 0x06,
    checksum = 0x07,
    not_verified = 0x08,
};

fn frameCrc(msg_type: u8, payload: []const u8) u32 {
    var len: [2]u8 = undefined;
    std.mem.writeInt(u16, &len, @intCast(payload.len), .little);

    var crc = std.hash.Crc32.init();
    crc.update(&.{msg_type});
    crc.update(&len);
    crc.update(payload);
    return crc.final();
}

/// Encodes a frame whose payload is the concatenation of `parts` into `buf`
/// and returns the used part of `buf`
pub fn encodeFrame(buf: []u8, msg_type: MsgType, parts: []const []const u8) []u8 {
    var len: usize = 0;
    for (parts) |part| {
        @memcpy(buf[header_len + len ..][0..part.len], part);
        len += part.len;
    }
    const payload = buf[header_len..][0..len];

    buf[0] = sync[0];
    buf[1] = sync[1];
    buf[2] = @intFromEnum(msg_type);
    std.mem.writeInt(u16, buf[3..5], @intCast(len), .little);
    std.mem.writeInt(u32, buf[header_len + len ..][0..4], frameCrc(buf[2], payload), .little);

    return buf[0 .. header_len + len + crc_len];
}

/// Recognizes the magic sequence in a byte stream
pub const MagicMatcher = struct {
    matched: usize = 0,

    /// Returns true when `byte` completes the magic sequence
    pub fn feed(self: *MagicMatcher, byte: u8) bool {
        if (byte == magic[self.matched]) {
            self.matched += 1;
        } else {
            // The first magic byte appears nowhere else in the sequence, so a
            // mismatch can only be the start of a new match
            self.matched = if (byte == magic[0]) 1 else 0;
        }

        if (self.matched == magic.len) {
            self.matched = 0;
            return true;
        }
        return false;
    }

    pub fn reset(self: *MagicMatcher) void {
        self.matched = 0;
    }
};

pub const Frame = struct {
    msg_type: MsgType,
    payload: []const u8,
};

pub const ParseEvent = union(enum) {
    frame: Frame,
    /// A complete frame arrived but its CRC did not match
    bad_crc: MsgType,
};

/// Reassembles frames from a byte stream, one byte at a time
pub const FrameParser = struct {
    state: State = .sync0,
    header: [3]u8 = undefined,
    header_pos: usize = 0,
    payload: [max_payload]u8 = undefined,
    payload_pos: usize = 0,
    crc: [crc_len]u8 = undefined,
    crc_pos: usize = 0,
    last_byte_us: u64 = 0,

    const State = enum { sync0, sync1, header, payload, crc };

    pub fn reset(self: *FrameParser) void {
        self.state = .sync0;
    }

    /// True while no frame is partially received
    pub fn idle(self: *const FrameParser) bool {
        return self.state == .sync0;
    }

    /// Drops a partially received frame that has stalled
    pub fn checkTimeout(self: *FrameParser, now_us: u64) void {
        if (self.state != .sync0 and now_us -| self.last_byte_us > frame_timeout_us) {
            self.state = .sync0;
        }
    }

    /// The returned frame payload is only valid until the next call
    pub fn feed(self: *FrameParser, byte: u8, now_us: u64) ?ParseEvent {
        self.checkTimeout(now_us);
        self.last_byte_us = now_us;

        switch (self.state) {
            .sync0, .sync1 => self.feedSync(byte),
            .header => {
                self.header[self.header_pos] = byte;
                self.header_pos += 1;
                if (self.header_pos < self.header.len) {
                    return null;
                }

                const len = self.payloadLen();
                if (len > max_payload) {
                    // Not a real frame. A sync could still start in the
                    // header bytes, so scan them again.
                    self.state = .sync0;
                    for (self.header) |b| {
                        self.feedSync(b);
                    }
                } else {
                    self.payload_pos = 0;
                    self.crc_pos = 0;
                    self.state = if (len == 0) .crc else .payload;
                }
            },
            .payload => {
                self.payload[self.payload_pos] = byte;
                self.payload_pos += 1;
                if (self.payload_pos == self.payloadLen()) {
                    self.state = .crc;
                }
            },
            .crc => {
                self.crc[self.crc_pos] = byte;
                self.crc_pos += 1;
                if (self.crc_pos < crc_len) {
                    return null;
                }

                self.state = .sync0;
                const msg_type: MsgType = @enumFromInt(self.header[0]);
                const payload = self.payload[0..self.payloadLen()];
                if (std.mem.readInt(u32, &self.crc, .little) != frameCrc(self.header[0], payload)) {
                    return .{ .bad_crc = msg_type };
                }
                return .{ .frame = .{ .msg_type = msg_type, .payload = payload } };
            },
        }
        return null;
    }

    fn feedSync(self: *FrameParser, byte: u8) void {
        switch (self.state) {
            .sync0 => {
                if (byte == sync[0]) self.state = .sync1;
            },
            .sync1 => {
                if (byte == sync[1]) {
                    self.state = .header;
                    self.header_pos = 0;
                } else if (byte != sync[0]) {
                    self.state = .sync0;
                }
            },
            else => unreachable,
        }
    }

    fn payloadLen(self: *const FrameParser) u16 {
        return std.mem.readInt(u16, self.header[1..3], .little);
    }
};

const testing = std.testing;

fn feedAll(parser: *FrameParser, bytes: []const u8) ?ParseEvent {
    var event: ?ParseEvent = null;
    for (bytes) |b| {
        if (parser.feed(b, 0)) |e| event = e;
    }
    return event;
}

test "encodes frames matching the reference CRC" {
    var buf: [max_frame_len]u8 = undefined;
    try testing.expectEqualSlices(
        u8,
        &.{ 0xa5, 0x5a, 0x01, 0x00, 0x00, 0x25, 0xb3, 0x83, 0xfe },
        encodeFrame(&buf, .ping, &.{}),
    );
    try testing.expectEqualSlices(
        u8,
        &.{ 0xa5, 0x5a, 0x80, 0x09, 0x00, 0x02, 0x01, 0x00, 0x00, 0x10, 0x01, 0x00, 0x00, 0x00, 0xf5, 0x4f, 0x5f, 0x5a },
        encodeFrame(&buf, .ok, &.{ &.{0x02}, &.{ 0x01, 0x00, 0x00, 0x10, 0x01, 0x00, 0x00, 0x00 } }),
    );
}

test "parses a frame preceded by noise" {
    var buf: [max_frame_len]u8 = undefined;
    const frame = encodeFrame(&buf, .data, &.{"hello"});

    var parser = FrameParser{};
    try testing.expectEqual(null, feedAll(&parser, &.{ 0x00, 0xA5, 0x13, 0xA5 }));
    const event = feedAll(&parser, frame).?;
    try testing.expectEqual(MsgType.data, event.frame.msg_type);
    try testing.expectEqualStrings("hello", event.frame.payload);
}

test "parses an empty frame" {
    var buf: [max_frame_len]u8 = undefined;
    var parser = FrameParser{};
    const event = feedAll(&parser, encodeFrame(&buf, .ping, &.{})).?;
    try testing.expectEqual(MsgType.ping, event.frame.msg_type);
    try testing.expectEqual(0, event.frame.payload.len);
}

test "reports a corrupted frame" {
    var buf: [max_frame_len]u8 = undefined;
    const frame = encodeFrame(&buf, .data, &.{"hello"});
    frame[7] ^= 0xFF;

    var parser = FrameParser{};
    try testing.expectEqual(ParseEvent{ .bad_crc = .data }, feedAll(&parser, frame).?);
    try testing.expect(parser.idle());
}

test "rejects an oversized length and resyncs inside the header" {
    var buf: [max_frame_len]u8 = undefined;
    const frame = encodeFrame(&buf, .ping, &.{});

    var parser = FrameParser{};
    // Bogus header: type 0x00, then a length of 0x5AA5 that starts the real frame
    try testing.expectEqual(null, feedAll(&parser, &.{ 0xA5, 0x5A, 0x00, 0xA5, 0x5A }));
    try testing.expectEqual(MsgType.ping, feedAll(&parser, frame[2..]).?.frame.msg_type);
}

test "drops a stalled frame" {
    var buf: [max_frame_len]u8 = undefined;
    const frame = encodeFrame(&buf, .data, &.{"hello"});

    var parser = FrameParser{};
    for (frame[0..6]) |b| _ = parser.feed(b, 0);
    parser.checkTimeout(frame_timeout_us + 1);
    try testing.expect(parser.idle());

    var event: ?ParseEvent = null;
    for (frame) |b| {
        if (parser.feed(b, frame_timeout_us + 2)) |e| event = e;
    }
    try testing.expectEqualStrings("hello", event.?.frame.payload);
}

test "matches the magic sequence after a partial match" {
    var matcher = MagicMatcher{};
    var found = false;
    for ("xPICPICHBT\r\n") |b| {
        found = matcher.feed(b);
    }
    try testing.expect(found);

    found = false;
    for ("PICHBT\rPICHB") |b| {
        found = found or matcher.feed(b);
    }
    try testing.expect(!found);
}
