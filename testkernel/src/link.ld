ENTRY(_start);

SECTIONS {
    . = 0x80000;
    .text : {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }
    .rodata : { *(.rodata .rodata.*) }
    .data : { *(.data .data.*) }
    .bss : { *(.bss .bss.*) }

    /DISCARD/ : {
        *(.comment*)
        *(.debug*)
        *(.note*)
        *(.eh_frame*)
    }
}
