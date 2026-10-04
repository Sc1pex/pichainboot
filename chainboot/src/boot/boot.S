.section .text.boot

.macro loadabs64 reg, symbol
    movz \reg, #:abs_g0_nc:\symbol
    movk \reg, #:abs_g1_nc:\symbol
    movk \reg, #:abs_g2_nc:\symbol
    movk \reg, #:abs_g3:\symbol
.endm

.global _start
_start:
    // The firmware passes the DTB address in x0, keep it for the loaded kernel
    mov x19, x0

    loadabs64 x1, __load_addr
    loadabs64 x2, __bin_start
    loadabs64 x3, __bin_end

1:
    cmp x2, x3
    b.eq 2f
    ldr x4, [x1], #8
    str x4, [x2], #8
    b 1b

2:
    // Invalidate the I-Cache
    ic iallu
    // Wait for all read/write operations to complete
    dsb sy
    // Flush the pipeline
    isb

    mov x0, #0x80000
    mov sp, x0

    mov x0, x19
    loadabs64 x1, __zig_init
    br x1
