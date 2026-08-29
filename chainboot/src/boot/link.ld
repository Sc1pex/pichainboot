__load_addr = 0x80000;

ENTRY(_start);

SECTIONS {
    . = 0x2000000;
    __bin_start = .;
    .text : { 
        KEEP(*(.text.boot))
        *(.text .text.*) 
    }
    .rodata : { *(.rodata .rodata.*) }
    .data : { *(.data .data.*) }
    . = ALIGN(16);
    __bin_end = .;

    .bss : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.*)
        . = ALIGN(16);
        __bss_end = .;
    }
    
    /DISCARD/ : {
        *(.comment*)
        *(.ARM.attributes)
        *(.debug*)
        *(.note*)
    }
}
