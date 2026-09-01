const mmio = @import("mmio.zig");

pub fn configureUART0() void {
    mmio.writeBarrier();
    defer mmio.readBarrier();

    configureUART0Functions();
    configureUART0Resistors();
}

const GPFSEL1 = mmio.GPIO_BASE + 0x04;
const GPIO_PUP_PDN_CNTRL_REG0 = mmio.GPIO_BASE + 0xE4;

fn configureUART0Functions() void {
    const alt0 = 0b100;
    var value = mmio.read(GPFSEL1);
    value &= ~@as(u32, 0b111_111 << 12);
    value |= alt0 << 12;
    value |= alt0 << 15;
    mmio.write(GPFSEL1, value);
}

fn configureUART0Resistors() void {
    var value = mmio.read(GPIO_PUP_PDN_CNTRL_REG0);
    value &= ~@as(u32, 0b11_11 << 28);
    mmio.write(GPIO_PUP_PDN_CNTRL_REG0, value);
}
