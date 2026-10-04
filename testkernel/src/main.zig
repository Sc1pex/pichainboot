//! A minimal kernel for testing the chainloader. It relies on the chainloader
//! having configured UART0, prints the DTB address it was booted with and then
//! echoes everything it receives.

comptime {
    asm (
        \\.section .text.boot
        \\.global _start
        \\_start:
        \\    mov x1, #0x80000
        \\    mov sp, x1
        \\    bl testkernel_main
        \\1:  wfe
        \\    b 1b
    );
}

const uart_base = 0xFE20_1000;
const dr: *volatile u32 = @ptrFromInt(uart_base + 0x00);
const fr: *volatile u32 = @ptrFromInt(uart_base + 0x18);
const fr_rxfe = 1 << 4;
const fr_txff = 1 << 5;

fn putc(c: u8) void {
    while (fr.* & fr_txff != 0) {}
    dr.* = c;
}

fn puts(s: []const u8) void {
    for (s) |c| putc(c);
}

fn putHex(v: u64) void {
    var shift: u7 = 64;
    while (shift > 0) {
        shift -= 4;
        putc("0123456789abcdef"[@intCast((v >> @intCast(shift)) & 0xf)]);
    }
}

export fn testkernel_main(dtb: u64) void {
    puts("testkernel: booted, dtb=0x");
    putHex(dtb);
    puts("\r\n");

    while (true) {
        while (fr.* & fr_rxfe != 0) {}
        putc(@truncate(dr.*));
    }
}
