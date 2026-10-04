const mmio = @import("mmio.zig");
const mr = @import("zregs");

const DR = mr.Register(32, struct {
    data: mr.Field(0, 8, .RW),
    fe: mr.Field(8, 1, .RO),
    pe: mr.Field(9, 1, .RO),
    be: mr.Field(10, 1, .RO),
    oe: mr.Field(11, 1, .RO),
});

const FR = mr.Register(32, struct {
    cts: mr.Field(0, 1, .RO),
    dsr: mr.Field(1, 1, .RO),
    dcd: mr.Field(2, 1, .RO),
    busy: mr.Field(3, 1, .RO),
    rxfe: mr.Field(4, 1, .RO),
    txff: mr.Field(5, 1, .RO),
    rxff: mr.Field(6, 1, .RO),
    txfe: mr.Field(7, 1, .RO),
});

const IBRD = mr.Register(32, struct {
    divint: mr.Field(0, 16, .RW),
});

const FBRD = mr.Register(32, struct {
    divfrac: mr.Field(0, 6, .RW),
});

const LCRH = mr.Register(32, struct {
    brk: mr.Field(0, 1, .RW),
    pen: mr.Field(1, 1, .RW),
    eps: mr.Field(2, 1, .RW),
    stp2: mr.Field(3, 1, .RW),
    fen: mr.Field(4, 1, .RW),
    wlen: mr.FieldWithValues(5, 2, .RW, enum(u2) {
        bits5 = 0b00,
        bits6 = 0b01,
        bits7 = 0b10,
        bits8 = 0b11,
    }),
    sps: mr.Field(7, 1, .RW),
});

const CR = mr.Register(32, struct {
    uarten: mr.Field(0, 1, .RW),
    lbe: mr.Field(7, 1, .RW),
    txe: mr.Field(8, 1, .RW),
    rxe: mr.Field(9, 1, .RW),
    rts: mr.Field(11, 1, .RW),
    rtsen: mr.Field(14, 1, .RW),
    ctsen: mr.Field(15, 1, .RW),
});

const ICR = mr.Register(32, struct {
    // all fields are write 1 to clear
    rimic: mr.Field(0, 1, .WO),
    ctsmic: mr.Field(1, 1, .WO),
    dcdmic: mr.Field(2, 1, .WO),
    dsrmic: mr.Field(3, 1, .WO),
    rxic: mr.Field(4, 1, .WO),
    txic: mr.Field(5, 1, .WO),
    rtic: mr.Field(6, 1, .WO),
    feic: mr.Field(7, 1, .WO),
    peic: mr.Field(8, 1, .WO),
    beic: mr.Field(9, 1, .WO),
    oeic: mr.Field(10, 1, .WO),
});

const Uart0 = mr.RegisterMap(&.{
    .{ "dr", 0x00, DR },
    .{ "fr", 0x18, FR },
    .{ "ibrd", 0x24, IBRD },
    .{ "fbrd", 0x28, FBRD },
    .{ "lcrh", 0x2C, LCRH },
    .{ "cr", 0x30, CR },
    .{ "icr", 0x44, ICR },
});

const uart0: *volatile Uart0 = @ptrFromInt(mmio.UART0_BASE);

pub fn configure() void {
    mmio.writeBarrier();
    defer mmio.readBarrier();

    uart0.cr.write(.{
        .uarten = 0,
        .lbe = 0,
        .txe = 0,
        .rxe = 0,
        .rts = 0,
        .rtsen = 0,
        .ctsen = 0,
    });
    // Wait for any pending transmission to complete
    while ((uart0.fr.read().busy) != 0) {}

    // Clear all interrupts
    uart0.icr.write(.{
        .rimic = 1,
        .ctsmic = 1,
        .dcdmic = 1,
        .dsrmic = 1,
        .rxic = 1,
        .txic = 1,
        .rtic = 1,
        .feic = 1,
        .peic = 1,
        .beic = 1,
        .oeic = 1,
    });

    const divisors = calculateBaudDivisors(UART_CLK, BAUD_RATE);
    uart0.ibrd.write(.{ .divint = divisors.ibrd });
    uart0.fbrd.write(.{ .divfrac = divisors.fbrd });

    // Set line control
    uart0.lcrh.modify(.{
        .wlen = .bits8,
        .fen = 1,
    });

    // Enable UART
    uart0.cr.modify(.{
        .uarten = 1,
        .txe = 1,
        .rxe = 1,
        .rtsen = 1,
        .ctsen = 1,
    });
}

pub fn write(byte: u8) void {
    mmio.writeBarrier();
    defer mmio.readBarrier();

    // Wait until transmit FIFO is not full
    while (uart0.fr.read().txff != 0) {}

    uart0.dr.write(.{ .data = byte });
}

pub fn read() u8 {
    mmio.writeBarrier();
    defer mmio.readBarrier();

    // Wait until receive FIFO is empty
    while (uart0.fr.read().rxfe != 0) {}

    return uart0.dr.read().data;
}

pub fn read_noblock() ?u8 {
    mmio.writeBarrier();
    defer mmio.readBarrier();

    // If receive FIFO is empty, there is no data
    if (uart0.fr.read().rxfe != 0) {
        return null;
    }

    return uart0.dr.read().data;
}

const UART_CLK = 48_000_000;
const BAUD_RATE = 115200;

const BaudDivisors = struct {
    ibrd: u16,
    fbrd: u6,
};

fn calculateBaudDivisors(comptime clock: u32, comptime baud: u32) BaudDivisors {
    const denom = 16 * baud;
    const ibrd = clock / denom;
    const remainder = clock % denom;
    const fbrd = ((remainder * 64) + (denom / 2)) / denom;
    return .{ .ibrd = ibrd, .fbrd = fbrd };
}
