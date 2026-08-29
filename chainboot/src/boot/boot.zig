const root = @import("root");

export fn __zig_init() noreturn {
    clean_bss();
    root.kernel_main();

    while (true) {}
}

extern var __bss_start: u64;
extern var __bss_end: u64;

fn clean_bss() void {
    const start_ptr: [*]u8 = @ptrCast(&__bss_start);
    const end_ptr: [*]u8 = @ptrCast(&__bss_end);
    const len = @intFromPtr(end_ptr) - @intFromPtr(start_ptr);

    @memset(start_ptr[0..len], 0);
}
