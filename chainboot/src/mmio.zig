pub const GPIO_BASE = 0xFE20_0000;
pub const UART0_BASE = 0xFE20_1000;

pub inline fn read(addr: usize) u32 {
    const addr_ptr: *volatile u32 = @ptrFromInt(addr);
    return addr_ptr.*;
}

pub inline fn write(addr: usize, value: u32) void {
    const addr_ptr: *volatile u32 = @ptrFromInt(addr);
    addr_ptr.* = value;
}

/// Ensures all preceding memory accesses complete before any subsequent peripheral access.
pub inline fn writeBarrier() void {
    // System-wide barrier for store (write) operations.
    asm volatile ("dsb st" ::: .{ .memory = true });
}

/// Ensures all preceding peripheral reads complete before any subsequent memory access.
pub inline fn readBarrier() void {
    // System-wide barrier for load (read) operations.
    asm volatile ("dsb ld" ::: .{ .memory = true });
}
