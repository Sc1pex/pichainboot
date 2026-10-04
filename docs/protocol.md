# pichainboot protocol

Protocol version: **1** (draft)

This document describes how the host tool talks to the chainloader running on
the Raspberry Pi over a UART byte stream. The stream may be a USB-serial
adapter or anything that behaves like one (for example a PTY exposed by
tcpuart).

- **Device**: the chainloader (`chainboot/`, Zig), running on the Pi.
- **Host**: the host tool (Rust TUI, planned).

## Goals

- Load an image into RAM and boot it (v1).
- Later: manage files on the SD card (write, compare by hash, delete, list),
  and boot images stored on the card.
- Survive line noise, resets on either side, and slow or bursty links.

## Conventions

- All multi-byte integers are **little-endian**.
- CRC32 is the IEEE 802.3 polynomial (`std.hash.Crc32` in Zig, `crc32fast` in
  Rust).
- The host is the only side that starts requests. The device sends exactly one
  response per request. The only unsolicited thing the device sends is the
  announce sequence, plus the `INFO` response that completes a handshake.
- **Every request is idempotent.** Sending the same request twice has the same
  effect as sending it once. That means a lost response can always be handled
  by resending the request, and no sequence numbers are needed. New commands
  must keep this property.

## Timing constants

| Name | Value | Meaning |
|---|---|---|
| `ANNOUNCE_INTERVAL` | 200 ms | Time between announce sequences sent by the device |
| `FRAME_TIMEOUT` | 500 ms | The device drops a partly received frame if no byte arrives for this long |
| `IDLE_TIMEOUT` | 10 s | In a session, the device goes back to announcing if no valid frame arrives for this long |
| `RESPONSE_TIMEOUT` | 1 s (host default) | How long the host waits for a response before resending. Configurable; use more over high-latency links |
| `MAX_RETRIES` | 5 (host default) | How many times the host resends a request before giving up |

## Handshake

The magic sequence is 8 bytes:

```
50 49 43 48 42 54 0D 0A    "PICHBT\r\n"
```

It's ASCII, so a plain terminal on the port shows `PICHBT` once per line and it
is obvious what is waiting there. The first byte doesn't appear anywhere else in
the sequence, so a simple matcher (on a mismatch, go back to the start and test
the current byte again) is correct.

```
device                               host
  |-- PICHBT\r\n ------------------->|   every ANNOUNCE_INTERVAL
  |-- PICHBT\r\n ------------------->|
  |<------------------- PICHBT\r\n --|   host echoes the sequence
  |-- frame: OK(INFO) -------------->|   session established
```

### Device

1. After boot, the device sends the magic sequence every `ANNOUNCE_INTERVAL`
   and checks incoming bytes for the same sequence.
2. When the full sequence arrives, the device stops announcing, enters a
   session, and sends an `OK` response for `INFO` (see below).
3. **The device looks for the magic sequence only while announcing.** In a
   session it ignores the sequence, because data being transferred can contain
   it. For example, an image of the chainloader itself contains it. If the
   device loses frame sync (a corrupted sync byte or length, or a stalled
   frame), it scans the following bytes only for the frame sync.
4. In a session, an `INFO` request clears all session state, including any
   active transfer. That is how a host starts a fresh session with a device
   that is already in one. `INFO` is a full frame protected by a CRC, so data
   can't trigger it by accident.
5. If no valid frame arrives for `IDLE_TIMEOUT`, the device ends the session
   and goes back to step 1. A host that wants to stay connected while idle
   sends `PING`.

A false match while announcing is harmless. No session exists yet, so all it
does is start one.

### Host

1. On startup, the host listens for one `ANNOUNCE_INTERVAL` plus a margin. If it
   sees the magic sequence, it echoes it. If not, the device may already be in
   a session, so the host sends an `INFO` request. Either way, receiving
   `OK(INFO)` means the session is established.
2. The host checks for the magic sequence only between frames, never inside a
   response payload.
3. Seeing the magic sequence during a session suggests the device has reset.
   The host echoes it. A device that really reset answers with `OK(INFO)`.
   A device still in its session ignores the echo, the host times out, and a
   `PING` shows the session is still alive.
4. If the host receives `OK(INFO)` while waiting for any other response, the
   session has restarted. Since every request is idempotent, the host restarts
   its current operation from the beginning. For a transfer, that means
   starting again from `MEM_WRITE`.

## Frames

Once the handshake is done, every message in both directions is a frame:

```
offset  size  field
0       2     sync     = A5 5A
2       1     type
3       2     len      payload length in bytes
5       len   payload
5+len   4     crc32    over type, len and payload
```

The receiver looks for `A5 5A`, reads the header, and reads `len` payload
bytes and the CRC.

- If `len` is larger than the device's `max_payload` (see `INFO`), the device
  treats the sync bytes as noise and starts looking for sync again at the next
  byte. It doesn't respond.
- If the CRC doesn't match, the device responds with `ERR BAD_FRAME`.
- If a frame stalls for longer than `FRAME_TIMEOUT`, the device drops it.

In every case where it gets no usable response, the host resends after
`RESPONSE_TIMEOUT`.

### Message types

Requests use `0x01`–`0x7F` and responses use `0x80`–`0xFF`.

| Type | Name | Direction |
|---|---|---|
| `0x01` | `PING` | host → device |
| `0x02` | `INFO` | host → device |
| `0x10` | `MEM_WRITE` | host → device |
| `0x11` | `DATA` | host → device |
| `0x12` | `END` | host → device |
| `0x13` | `BOOT` | host → device |
| `0x20`–`0x2F` | reserved for SD card commands | host → device |
| `0x80` | `OK` | device → host |
| `0xFF` | `ERR` | device → host |

### Responses

```
OK   payload: req_type u8, data...
ERR  payload: req_type u8, code u8
```

`req_type` is the type of the request being answered. For the `OK` sent after a
handshake, `req_type` is `INFO`.

| Code | Name | Meaning |
|---|---|---|
| `0x01` | `BAD_FRAME` | The CRC didn't match. Resend |
| `0x02` | `UNSUPPORTED` | The device doesn't know this request type |
| `0x03` | `BAD_PAYLOAD` | The payload length or contents are invalid for this request |
| `0x04` | `BAD_ADDRESS` | The memory range isn't allowed (see [Memory rules](#memory-rules)) |
| `0x05` | `NO_TRANSFER` | `DATA` or `END` arrived with no transfer active |
| `0x06` | `OUT_OF_RANGE` | `DATA` would write past the end of the transfer |
| `0x07` | `CHECKSUM` | The transferred data doesn't match the CRC given in `END` |
| `0x08` | `NOT_VERIFIED` | `BOOT` points outside the last verified transfer |
| `0x10`–`0x1F` | reserved for SD card errors | |

## Commands (v1)

### `PING` (0x01)

Request payload: empty. Response: `OK`, no data. Use it as a keepalive.

### `INFO` (0x02)

Request payload: empty. Clears all session state, including any active or
verified transfer (see [Handshake](#handshake)). Response `OK` data:

```
proto_version  u16   1
max_payload    u16   largest frame payload the device accepts
capabilities   u32   bit 0: memory load + boot
                     bit 1: SD card file commands (not in v1)
```

The host must check `proto_version` and must not send commands whose
capability bit is clear.

### Transfers: `MEM_WRITE` (0x10), `DATA` (0x11), `END` (0x12)

Moving bytes to the device always works the same way: a command that starts a
transfer, any number of `DATA` chunks, then `END`. `MEM_WRITE` is the only
starting command in v1. A future `FILE_WRITE` will reuse `DATA` and `END`
unchanged.

```
MEM_WRITE  payload: addr u64, size u32
DATA       payload: offset u32, bytes[len - 4]
END        payload: crc32 u32       CRC32 of all `size` bytes of the transfer
```

- `MEM_WRITE` checks the range against the [memory rules](#memory-rules) and
  starts a new transfer, replacing any active one. Sending it again with the
  same arguments just starts that transfer over.
- `DATA` writes `bytes` at `addr + offset`. Chunks may arrive in any order and
  may repeat. A repeated chunk writes the same bytes again. Each chunk can be at
  most `max_payload - 4` bytes.
- `END` computes the CRC32 over the whole destination range and compares it to
  the one given. On a match, the transfer becomes *verified* and the device
  replies `OK`. Any later `DATA` or `MEM_WRITE` clears the verification, since
  memory may have changed. Otherwise it replies `ERR CHECKSUM`, and the host can resend
  some or all chunks and send `END` again.

The device doesn't track which chunks have arrived. The final CRC is what
catches missing or corrupted data.

### `BOOT` (0x13)

```
BOOT  payload: entry u64
```

`entry` must lie inside the most recent verified transfer, or the device
replies `ERR NOT_VERIFIED`. Otherwise the device:

1. Replies `OK` and waits until the UART has finished transmitting it.
2. Invalidates the instruction cache.
3. Jumps to `entry` with `x0` = the DTB address the firmware passed to the
   chainloader and `x1`–`x3` = 0, as the arm64 Linux boot protocol expects.

After it receives `OK`, the host stops using frames and becomes a plain serial
console. If the `OK` is lost, the host may resend `BOOT`. Should the device
have already jumped, those bytes go to the new kernel as console input. The
host should report the boot as unconfirmed rather than keep retrying.

### Memory rules

A `MEM_WRITE` range `[addr, addr + size)` is accepted only if:

- it is inside `[0x80000, RAM_TOP)`, where v1 uses a conservative
  `RAM_TOP = 0x3B400000` (low memory with the default GPU split). Later
  versions can ask the firmware mailbox for the real value. Everything below
  `0x80000` is excluded, which protects the secondary-core spin tables and the
  loader's stack;
- it doesn't overlap the chainloader image and its `.bss`;
- it doesn't overlap the DTB the firmware passed in (its size is the
  `totalsize` field in the DTB header).

Otherwise the device replies `ERR BAD_ADDRESS`.

## Typical v1 session

```
device                                     host
  |-- PICHBT\r\n ------------------------->|
  |<------------------------- PICHBT\r\n --|
  |-- OK(INFO, v1, 4096, caps=1) --------->|
  |<--------- MEM_WRITE(0x80000, 81920) ---|
  |-- OK(MEM_WRITE) ---------------------->|
  |<--------------- DATA(0, 4092 bytes) ---|
  |-- OK(DATA) --------------------------->|
  |          ... 20 more chunks ...        |
  |<----------------------- END(crc32) ----|
  |-- OK(END) ---------------------------->|
  |<-------------------- BOOT(0x80000) ----|
  |-- OK(BOOT) --------------------------->|
  |  jumps to 0x80000                      |  switches to console
```

## Planned: SD card commands

This section isn't part of v1. It records the intended design so that v1
doesn't make it harder.

| Type | Name | Payload | Notes |
|---|---|---|---|
| `0x20` | `FILE_WRITE` | flags u8, size u32, path | Starts a transfer into a file; uses `DATA`/`END` |
| `0x21` | `FILE_HASH` | path | `OK` with the file's hash, or `ERR NOT_FOUND` |
| `0x22` | `FILE_DELETE` | flags u8, path | |
| `0x23` | `LIST` | path | Directory listing |
| `0x24` | `BOOT_FILE` | addr u64, path | Load a file from the card into RAM and boot it |

**Protected files.** The device keeps a list of files that the Pi needs in
order to boot, or to reach this loader: its own image (normally
`kernel8.img`), `config.txt`, `start4.elf`, `fixup4.dat`, and the board's
DTB. A write or delete that touches one of them fails with `ERR PROTECTED`
unless the request sets the `FORCE` flag (bit 0). When the host gets
`PROTECTED`, it asks the user to confirm and only then resends with `FORCE`.
The device enforces this itself, so a buggy host can't skip the check.

Open questions for that phase:
- Which hash `FILE_HASH` returns (CRC32 is cheap; SHA-256 is safer and
  `std.crypto` works freestanding).
- Whether `FILE_WRITE` writes to a temporary file and renames it at `END`, so
  an interrupted transfer never leaves a half-written file in place.
- How paths are encoded (length-prefixed UTF-8, FAT 8.3 vs. long names).
