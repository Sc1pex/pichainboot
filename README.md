# pichainboot

A UART chainloader for the Raspberry Pi 4, with a host tool to load and boot
kernels over a serial line. Put the chainloader on the SD card once, then boot
new builds of your kernel without touching the card again.

```
chainboot/    the chainloader, runs on the Pi (Zig)
host/         the host tool: protocol library and TUI (Rust)
testkernel/   tiny kernel used by the end-to-end tests
docs/         protocol specification
```

The protocol is described in [docs/protocol.md](docs/protocol.md).

## Status

Version 1: load an image into RAM and boot it. Managing files on the SD card is
planned. Everything is tested in QEMU's `raspi4b` machine; real hardware has
not been tested yet.

## Building

Requires Zig 0.16.0 and a stable Rust toolchain.

```sh
cd chainboot && zig build    # produces zig-out/bin/kernel8.img
cd host && cargo build --release
```

## Setting up the Pi

1. Copy `chainboot/zig-out/bin/kernel8.img` to the boot partition of the SD
   card, next to the Raspberry Pi firmware files.
2. In `config.txt`:
   ```ini
   arm_64bit=1
   # Keep Bluetooth off the PL011 UART that the chainloader uses
   dtoverlay=disable-bt
   ```
3. Wire a USB-serial adapter **with hardware flow control** to the GPIO header:

   | Pi | GPIO | Adapter |
   |---|---|---|
   | TXD0 | 14 (pin 8) | RX |
   | RXD0 | 15 (pin 10) | TX |
   | CTS0 | 16 (pin 36) | RTS |
   | RTS0 | 17 (pin 11) | CTS |
   | GND | pin 6 | GND |

   The chainloader runs UART0 at 115200 baud with RTS/CTS enabled. It only
   transmits while CTS is asserted, so CTS must be connected.

## Using the host tool

```sh
pichainboot -p /dev/ttyUSB0 path/to/kernel8.img
```

The TUI waits for the Pi to announce itself (power it on or reset it), then:

| Key | Action |
|---|---|
| `b` | Load and boot the image. It is read from disk again every time. |
| `a` | Toggle auto boot: boot every time the Pi connects, e.g. after a reset |
| `c` | Clear the console |
| `q` | Quit |
| `PgUp` / `PgDn` | Scroll the console |

After a boot, everything you type goes to the Pi. Use `Ctrl-A` followed by one
of the keys above for commands, `Ctrl-A Ctrl-A` to send a literal `Ctrl-A`,
and `Shift-PgUp`/`Shift-PgDn` to scroll.

With `--auto`, the development loop is: rebuild your kernel, press reset on the
Pi, and the new build boots.

`--headless` skips the TUI: it boots once and then acts as a plain serial
console on stdin/stdout (quit with `Ctrl-]`). Combine it with `--auto` to boot
again after every reset.

Other options: `--addr` and `--entry` set the load and entry addresses (default
`0x80000`), `--baud`, `--flow-control none`, `--timeout-ms` and `--retries`
for slow links, and `--log-file` for debug logs. See `pichainboot --help`.

## Over Wi-Fi with tcpuart

[tcpuart](https://github.com/Sc1pex/tcpuart) bridges the Pi's UART over Wi-Fi
through an ESP32. Build the host tool with the `tcpuart` feature to use its
connections by name and reset the Pi remotely:

```sh
cargo build --release --features tcpuart
pichainboot -p tcpuart:pi path/to/kernel8.img
```

The PTY of the connection is looked up through the tcpuart daemon (socket
`/tmp/tcpuart.sock`, or `--tcpuart-socket` / `TCPUART_SOCKET`). The TUI gains
an `r` key (`Ctrl-A r` in the console) that resets the Pi through the ESP32's
reset line, and `--reset` resets it at startup. With both, rebuilding and
running

```sh
pichainboot --headless --reset -p tcpuart:pi path/to/kernel8.img
```

boots the new build without touching the Pi. Remote reset needs the ESP32's
reset GPIO wired to the Pi's RUN pin and enabled in the tcpuart firmware.

## Development with QEMU

QEMU's `raspi4b` machine runs the chainloader unmodified:

```sh
qemu-system-aarch64 -M raspi4b -kernel chainboot/zig-out/bin/kernel8.img \
    -serial tcp:127.0.0.1:4444,server=on,wait=off -display none
pichainboot -p tcp:127.0.0.1:4444 testkernel/zig-out/bin/testkernel.img
```

## Tests

```sh
cd chainboot && zig build test    # device protocol logic, runs on the host
cd host && cargo test             # host protocol and TUI
cd host && cargo test -p pichainboot --test qemu -- --ignored
cd host && cargo test -p pichainboot --features tcpuart --test tcpuart -- --include-ignored
```

The last command runs end-to-end tests against the real chainloader in QEMU,
including a link that corrupts and drops bytes and a reset in the middle of a
transfer. The tcpuart tests simulate the daemon with a PTY bridged to QEMU and
a fake control socket. These tests need `qemu-system-aarch64` and `zig`.
