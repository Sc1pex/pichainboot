const mmio = @import("mmio.zig");

pub fn configure() void {
    mmio.writeBarrier();
    defer mmio.readBarrier();

    mmio.write(CR, 0);
    // Wait for any pending transmission to complete
    while ((mmio.read(FR) & (1 << 3)) != 0) {}

    // Clear all interrupts
    mmio.write(ICR, 0x7FF);

    const divisors = calculateBaudDivisors(UART_CLK, BAUD_RATE);
    mmio.write(IBRD, divisors.ibrd);
    mmio.write(FBRD, divisors.fbrd);

    // Set line control
    // WLEN 8 bits
    // Enable FIFOs
    mmio.write(LCRH, (0b11 << 5) | (1 << 4));

    // Enable UART
    mmio.write(CR, (1 << 0) | (1 << 8) | (1 << 9));
}

pub fn write(byte: u8) void {
    mmio.writeBarrier();
    defer mmio.readBarrier();

    // Wait until transmit FIFO is not full
    while ((mmio.read(FR) & (1 << 5)) != 0) {}

    mmio.write(DR, byte);
}

pub fn read() u8 {
    mmio.writeBarrier();
    defer mmio.readBarrier();

    // Wait until receive FIFO is empty
    while ((mmio.read(FR) & (1 << 4)) != 0) {}

    return @as(u8, @truncate(mmio.read(DR)));
}

pub fn read_noblock() ?u8 {
    mmio.writeBarrier();
    defer mmio.readBarrier();

    // If receive FIFO is empty, there is no data
    if ((mmio.read(FR) & (1 << 4)) != 0) {
        return null;
    }

    return @as(u8, @truncate(mmio.read(DR)));
}

const UART_CLK = 48_000_000;
const BAUD_RATE = 115200;

const BaudDivisors = struct {
    ibrd: u32,
    fbrd: u32,
};

fn calculateBaudDivisors(comptime clock: u32, comptime baud: u32) BaudDivisors {
    const denom = 16 * baud;
    const ibrd = clock / denom;
    const remainder = clock % denom;
    const fbrd = ((remainder * 64) + (denom / 2)) / denom;
    return .{ .ibrd = ibrd, .fbrd = fbrd };
}

const DR = mmio.UART0_BASE + 0x00;
const FR = mmio.UART0_BASE + 0x18;
const IBRD = mmio.UART0_BASE + 0x24;
const FBRD = mmio.UART0_BASE + 0x28;
const LCRH = mmio.UART0_BASE + 0x2C;
const CR = mmio.UART0_BASE + 0x30;
const ICR = mmio.UART0_BASE + 0x44;
