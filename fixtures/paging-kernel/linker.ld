/* PrincessIDE paging kernel — linker script.
 *
 * A single loadable image based at 1 MiB (the conventional x86 kernel load
 * address), split into an RX text segment and an RW data segment.  The boot
 * code builds its page tables in .bss and identity-maps the whole image with
 * 4 KiB pages, so the loader only has to place the segments.
 *
 * The Multiboot headers must live in the first bytes of the *file*, so they are
 * the first sections placed; p_offset and p_vaddr are congruent modulo 8, so an
 * 8-byte aligned vaddr is also an 8-byte aligned file offset, which is what the
 * Multiboot 2 header search requires.
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

    /* .bss holds the page tables and the IDT; it is placed on its own 4 KiB
     * boundary (and the C objects inside it are 4 KiB aligned) so CR3 values
     * are always well formed. */
    . = ALIGN(4096);
    .bss :
    {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /* 64 KiB boot stack, 16-byte aligned as required by the SysV AMD64 ABI. */
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
