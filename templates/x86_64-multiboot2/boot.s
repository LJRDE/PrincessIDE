/* PrincessIDE kernel project template — x86_64 Multiboot2 boot entry.
 *
 * This file is meant to be read and copied.  It does the *minimum* needed to
 * get from the GRUB / Multiboot2 handoff into 64-bit long mode and into
 * kernel_main(); it deliberately has no interrupts, no paging management and
 * no memory allocator.
 *
 * Bootloader handoff state (Multiboot2, 32-bit protected mode, paging off):
 *   EAX = 0x36D76289  Multiboot2 boot magic
 *   EBX = physical address of the Multiboot2 information structure
 *
 * What happens below:
 *   1. stash the handoff registers on our own variables,
 *   2. load a flat GDT and normalise the segment registers,
 *   3. build a 1 GiB identity map from 2 MiB pages,
 *   4. enable PAE + LME + paging and far-jump into 64-bit mode,
 *   5. set up a stack and call kernel_main(magic, info).
 *
 * Assembled with GNU as (see the Makefile: `as --64`).  Section names are
 * consumed by linker.ld.
 */

/* GDT selector values (byte offsets into the table at the bottom). */
.set GDT_NULL,   0x00
.set GDT_CODE64, 0x08
.set GDT_DATA64, 0x10
.set GDT_CODE32, 0x18
.set GDT_DATA32, 0x20

/* ----------------------------------------------------------- Multiboot2 header
 * The header must be 8-byte aligned and land in the first 32 KiB of the image;
 * linker.ld therefore places the .multiboot2 section first.
 */
.section .multiboot2, "a"
.align 8
mb2_header:
    .long 0xE85250D6                    /* magic                              */
    .long 0                             /* architecture: i386 protected mode  */
    .long mb2_header_end - mb2_header   /* header length                      */
    .long -(0xE85250D6 + 0 + (mb2_header_end - mb2_header))  /* checksum       */
    /* Information request tag: ask the loader for the memory map. */
    .align 8
    .short 4                            /* type: information request          */
    .short 0                            /* flags                              */
    .long 16                            /* tag size                           */
    .long 6                             /* requested tag: memory map          */
    /* End tag. */
    .align 8
    .short 0
    .short 0
    .long 8
mb2_header_end:

/* ------------------------------------------------------------- 32-bit entry */
.section .text.boot, "ax"
.code32
.global _start
.type _start, @function
_start:
    cli
    cld

    /* Keep the Multiboot2 handoff registers before we clobber EAX/EBX. */
    movl    %eax, mb_magic
    movl    %ebx, mb_info

    /* Normalise the code segment onto our own flat 32-bit descriptor. */
    lgdt    gdt_ptr
    ljmp    $GDT_CODE32, $.Lflat32
.Lflat32:
    movw    $GDT_DATA32, %ax
    movw    %ax, %ds
    movw    %ax, %es
    movw    %ax, %ss
    movw    %ax, %fs
    movw    %ax, %gs

    /* ------------------------------------------------ identity page tables */
    /* PML4[0] -> PDPT */
    movl    $pdpt, %eax
    orl     $0x3, %eax                  /* present + writable                 */
    movl    %eax, pml4
    movl    $0, pml4 + 4

    /* PDPT[0] -> PD */
    movl    $pd, %eax
    orl     $0x3, %eax
    movl    %eax, pdpt
    movl    $0, pdpt + 4

    /* PD: 512 * 2 MiB = 1 GiB identity mapped, present + writable + huge. */
    xorl    %ecx, %ecx
.Lfill_pd:
    movl    %ecx, %eax
    shll    $21, %eax                   /* index * 0x200000                   */
    orl     $0x83, %eax                 /* P | RW | PS (2 MiB page)           */
    movl    %eax, pd(,%ecx,8)
    movl    $0, pd + 4(,%ecx,8)
    incl    %ecx
    cmpl    $512, %ecx
    jb      .Lfill_pd

    /* ------------------------------------------------------------ long mode */
    movl    %cr4, %eax
    orl     $0x20, %eax                 /* CR4.PAE                            */
    movl    %eax, %cr4

    movl    $pml4, %eax
    movl    %eax, %cr3

    movl    $0xC0000080, %ecx           /* IA32_EFER                          */
    rdmsr
    orl     $0x100, %eax                /* EFER.LME                           */
    wrmsr

    movl    %cr0, %eax
    orl     $0x80000001, %eax           /* CR0.PG | CR0.PE                    */
    movl    %eax, %cr0

    /* Compatibility mode -> 64-bit mode: far jump to a 64-bit code segment. */
    ljmp    $GDT_CODE64, $.Llong_entry

/* ------------------------------------------------------------- 64-bit entry */
.code64
.Llong_entry:
    movw    $GDT_DATA64, %ax
    movw    %ax, %ds
    movw    %ax, %es
    movw    %ax, %ss
    movw    %ax, %fs
    movw    %ax, %gs

    movq    $stack_top, %rsp
    xorq    %rbp, %rbp

    movl    mb_magic(%rip), %edi        /* arg 1: Multiboot2 magic            */
    movl    mb_info(%rip), %esi         /* arg 2: boot info physical address  */
    call    kernel_main

    /* kernel_main() should never return; halt safely if it does. */
.Lhang:
    cli
    hlt
    jmp     .Lhang
.size _start, . - _start

/* --------------------------------------------------------------------- GDT */
.section .rodata
.align 8
gdt:
    .quad 0x0000000000000000            /* 0x00 null                          */
    .quad 0x00AF9A000000FFFF            /* 0x08 code64: L=1, DPL0, present    */
    .quad 0x00CF92000000FFFF            /* 0x10 data64: RW, present           */
    .quad 0x00CF9A000000FFFF            /* 0x18 code32: G=1, D=1, present     */
    .quad 0x00CF92000000FFFF            /* 0x20 data32: G=1, D=1, present     */
gdt_end:

.align 8
gdt_ptr:
    .word gdt_end - gdt - 1             /* limit                              */
    .quad gdt                           /* base (low 4 bytes used in 32-bit)  */

/* ---------------------------------------------------------- .bss and stack */
.section .bss
.align 8
.global mb_magic
mb_magic:
    .long 0
.global mb_info
mb_info:
    .long 0

.align 4096
pml4:
    .space 4096
.align 4096
pdpt:
    .space 4096
.align 4096
pd:
    .space 4096

/* 64 KiB boot stack; the SysV AMD64 ABI wants 16-byte alignment. */
.section .stack, "aw", @nobits
.align 16
stack_bottom:
    .space 65536
.global stack_top
stack_top:

/* Tell the linker this object needs no executable stack. */
.section .note.GNU-stack, "", @progbits
