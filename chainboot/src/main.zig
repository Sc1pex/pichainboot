comptime {
    _ = @import("boot/boot.zig");
}

const std = @import("std");
const gpio = @import("gpio.zig");
const uart = @import("uart.zig");
const timer = @import("timer.zig");
const session = @import("session.zig");

const Range = session.Range;

/// Memory images can be loaded into. Everything below 0x80000 holds the
/// secondary core spin tables and our stack. The top is the end of low memory
/// with the default GPU memory split.
const usable_memory = Range{ .start = 0x80000, .end = 0x3B40_0000 };

extern var __bin_start: u8;
extern var __bss_end: u8;

const HwPlatform = struct {
    pub fn readByte(_: *HwPlatform) ?u8 {
        return uart.read_noblock();
    }

    pub fn write(_: *HwPlatform, bytes: []const u8) void {
        uart.writeAll(bytes);
    }

    pub fn canAnnounce(_: *HwPlatform) bool {
        // The FIFO is 16 bytes deep, so the whole magic fits when it is empty
        return uart.txEmpty();
    }

    pub fn flush(_: *HwPlatform) void {
        uart.flush();
    }

    pub fn nowUs(_: *HwPlatform) u64 {
        return timer.nowUs();
    }

    pub fn memWrite(_: *HwPlatform, addr: u64, bytes: []const u8) void {
        const dst: [*]u8 = @ptrFromInt(addr);
        @memcpy(dst[0..bytes.len], bytes);
    }

    pub fn memCrc(_: *HwPlatform, addr: u64, len: u32) u32 {
        const src: [*]const u8 = @ptrFromInt(addr);
        return std.hash.Crc32.hash(src[0..len]);
    }
};

pub fn kernel_main(dtb: usize) void {
    gpio.configureUART0();
    uart.configure();

    var reserved: [2]Range = undefined;
    var num_reserved: usize = 0;
    reserved[num_reserved] = .{ .start = @intFromPtr(&__bin_start), .end = @intFromPtr(&__bss_end) };
    num_reserved += 1;
    if (dtbRange(dtb)) |range| {
        reserved[num_reserved] = range;
        num_reserved += 1;
    }

    var platform = HwPlatform{};
    var s = session.Session(HwPlatform).init(&platform, .{
        .usable = usable_memory,
        .reserved = reserved[0..num_reserved],
    });

    while (true) {
        if (s.poll()) |entry| {
            jump(entry, dtb);
        }
    }
}

/// Returns the memory occupied by the DTB, if `addr` points to one
fn dtbRange(addr: usize) ?Range {
    const fdt_magic = 0xd00dfeed;
    if (addr == 0 or addr > usable_memory.end - 8) {
        return null;
    }

    // Read byte by byte, the DTB is not guaranteed to be aligned
    const header: *const [8]u8 = @ptrFromInt(addr);
    if (std.mem.readInt(u32, header[0..4], .big) != fdt_magic) {
        return null;
    }
    const size = std.mem.readInt(u32, header[4..8], .big);
    return .{ .start = addr, .end = addr + size };
}

/// Jumps to a loaded image using the arm64 Linux boot protocol
fn jump(entry: u64, dtb: usize) noreturn {
    asm volatile (
        \\ic iallu
        \\dsb sy
        \\isb
        \\br %[entry]
        :
        : [entry] "r" (entry),
          [dtb] "{x0}" (dtb),
          [x1] "{x1}" (0),
          [x2] "{x2}" (0),
          [x3] "{x3}" (0),
        : .{ .memory = true });
    unreachable;
}
