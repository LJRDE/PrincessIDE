/* PrincessIDE kernel project template — linker script.
 *
 * One R E segment (code + read-only data) and one RW segment, both based at the
 * conventional 1 MiB kernel load address.  The Multiboot2 header must be inside
 * the first 32 KiB of the *file* and 8-byte aligned, so the .multiboot2 section
 * is placed first and the image start is page-aligned.
 */
OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)

PHDRS
{
    text PT_LOAD FLAGS(5);   /* R E */
    data PT_LOAD FLAGS(6);   /* RW  */
}

SECTIONS
{
    . = 1M;

    .multiboot : ALIGN(8)
    {
        KEEP(*(.multiboot))
        KEEP(*(.multiboot2))
    } :text

    .text : ALIGN(16)
    {
        *(.text.boot)
        *(.text .text.*)
    } :text

    .rodata : ALIGN(16)
    {
        *(.rodata .rodata.*)
    } :text

    .data : ALIGN(4096)
    {
        *(.data .data.*)
    } :data

    .bss : ALIGN(4096)
    {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /* Boot stack (see boot.s). */
    . = ALIGN(16);
    .stack (NOLOAD) :
    {
        *(.stack)
    } :data

    /DISCARD/ :
    {
        *(.comment)
        *(.note.*)
        *(.eh_frame*)
    }
}
