/* Linker script for the boot-path-A probe (ELF32, multiboot 1). */
OUTPUT_FORMAT(elf32-i386)
ENTRY(_start)
SECTIONS
{
    . = 1M;
    .multiboot : { KEEP(*(.multiboot)) }
    .text      : { *(.text) *(.rodata) }
}
