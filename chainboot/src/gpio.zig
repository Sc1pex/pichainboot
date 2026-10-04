const mmio = @import("mmio.zig");
const mr = @import("zregs");

pub const PinFunction = enum(u3) {
    input = 0b000,
    output = 0b001,
    alt0 = 0b100,
    alt1 = 0b101,
    alt2 = 0b110,
    alt3 = 0b111,
    alt4 = 0b011,
    alt5 = 0b010,
};

pub const PinPull = enum(u2) {
    none = 0b00,
    up = 0b01,
    down = 0b10,
    reserved = 0b11,
};

const GPFSEL1 = mr.Register(32, struct {
    fsel10_13: mr.Field(0, 12, .RW),
    fsel14: mr.FieldWithValues(12, 3, .RW, PinFunction),
    fsel15: mr.FieldWithValues(15, 3, .RW, PinFunction),
    fsel16: mr.FieldWithValues(18, 3, .RW, PinFunction),
    fsel17: mr.FieldWithValues(21, 3, .RW, PinFunction),
    fsel18_19: mr.Field(24, 6, .RW),
});

const PUP_PDN_CNTRL_REG0 = mr.Register(32, struct {
    pull0_13: mr.Field(0, 28, .RW),
    pull14: mr.FieldWithValues(28, 2, .RW, PinPull),
    pull15: mr.FieldWithValues(30, 2, .RW, PinPull),
});

const PUP_PDN_CNTRL_REG1 = mr.Register(32, struct {
    pull16: mr.FieldWithValues(0, 2, .RW, PinPull),
    pull17: mr.FieldWithValues(2, 2, .RW, PinPull),
    pull18_31: mr.Field(4, 28, .RW),
});

const GPIORegs = mr.RegisterMap(&.{
    .{ "gpfsel1", 0x04, GPFSEL1 },
    .{ "pup_pdn_cntrl_reg0", 0xE4, PUP_PDN_CNTRL_REG0 },
    .{ "pup_pdn_cntrl_reg1", 0xE8, PUP_PDN_CNTRL_REG1 },
});

const gpio: *volatile GPIORegs = @ptrFromInt(mmio.GPIO_BASE);

pub fn configureUART0() void {
    mmio.writeBarrier();
    defer mmio.readBarrier();

    gpio.gpfsel1.modify(.{
        .fsel14 = .alt0,
        .fsel15 = .alt0,
        .fsel16 = .alt3,
        .fsel17 = .alt3,
    });

    gpio.pup_pdn_cntrl_reg0.modify(.{
        .pull14 = .none,
        .pull15 = .none,
    });

    gpio.pup_pdn_cntrl_reg1.modify(.{
        .pull16 = .none,
        .pull17 = .none,
    });
}
