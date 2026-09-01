comptime {
    _ = @import("boot/boot.zig");
}

const gpio = @import("gpio.zig");
const uart = @import("uart.zig");

pub fn kernel_main() void {
    gpio.configureUART0();
    uart.configure();

    uart.write('H');
    uart.write('e');
    uart.write('l');
    uart.write('l');
    uart.write('o');
    uart.write(' ');
    uart.write('w');
    uart.write('o');
    uart.write('r');
    uart.write('l');
    uart.write('d');
    uart.write('!');
    uart.write('\n');

    while (true) {
        const char = uart.read();
        uart.write(char);
    }
}
