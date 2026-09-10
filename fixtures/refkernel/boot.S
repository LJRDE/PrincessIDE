/* PrincessIDE reference kernel — boot entry.
 *
 * Boot path: Multiboot.  The image carries both a Multiboot 1 header (used by
 * `qemu-system-x86_64 -kernel`) and a Multiboot 2 header (used by a GRUB
 * produced ISO).  Either loader enters the image in 32-bit protected mode with
 * paging off, EAX = boot magic and EBX = address of the boot information
 * structure.  This file:
 *
 *   1. stashes the multiboot handoff registers,
 *   2. installs its own flat GDT,
 *   3. builds a 1 GiB identity map out of 2 MiB pages,
 *   4. enables PAE + LME + paging and far-jumps into 64-bit long mode,
 *   5. calls kernel_main().
 *
 * Everything here is deliberately freestanding: no libc, no third party code.
 */

/* GDT selector values (byte offsets into the table below). */
.set GDT_NULL,   0x00
.set GDT_CODE64, 0x08
.set GDT_DATA64, 0x10
.set GDT_CODE32, 0x18
.set GDT_DATA32, 0x20

/* ------------------------------------------------------------------ headers */

.section .multiboot, "a"
.align 8
    /* Multiboot 1 header — 4-byte aligned, inside the first 8 KiB. */
    .long 0x1BADB002                    /* magic                              */
    .long 0x00000003                    /* flags: page-align + memory info    */
    .long -(0x1BADB002 + 0x00000003)    /* checksum                           */

.section .multiboot2, "a"
.align 8
    /* Multiboot 2 header — 8-byte aligned, inside the first 32 KiB. */
mb2_header:
    .long 0xE85250D6                    /* magic                              */
    .long 0                             /* architecture: i386 (protected mode)*/
    .long mb2_header_end - mb2_header   /* header length                      */
    .long -(0xE85250D6 + 0 + (mb2_header_end - mb2_header))
    /* Information request tag: ask for the memory map. */
    .align 8
    .short 4                            /* type: information request          */
    .short 0                            /* flags                              */
    .long 16                            /* size                               */
    .long 6                             /* request: memory map                */
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

    /* Multiboot handoff: EAX = magic, EBX = boot information address. */
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

    /* -------------------------------------------------- identity page tables  */
    /* PML4[0] -> PDPT */
    movl    $pdpt, %eax
    orl     $0x3, %eax
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
    shll    $21, %eax                   /* index * 0x200000 */
    orl     $0x83, %eax                 /* P | RW | PS (2 MiB) */
    movl    %eax, pd(,%ecx,8)
    movl    $0, pd + 4(,%ecx,8)
    incl    %ecx
    cmpl    $512, %ecx
    jb      .Lfill_pd

    /* ------------------------------------------------------------- long mode  */
    movl    %cr4, %eax
    orl     $0x20, %eax                 /* CR4.PAE */
    movl    %eax, %cr4

    movl    $pml4, %eax
    movl    %eax, %cr3

    movl    $0xC0000080, %ecx           /* IA32_EFER */
    rdmsr
    orl     $0x100, %eax                /* EFER.LME  */
    wrmsr

    movl    %cr0, %eax
    orl     $0x80000001, %eax           /* CR0.PG | CR0.PE */
    movl    %eax, %cr0

    /* Compatibility mode -> 64-bit mode via a far jump to a 64-bit code seg. */
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

    movl    mb_magic(%rip), %edi        /* arg 1: multiboot magic   */
    movl    mb_info(%rip), %esi         /* arg 2: boot info address */
    call    kernel_main

.Lhang:
    cli
    hlt
    jmp     .Lhang
.size _start, . - _start

/* ------------------------------------------------------------------------ GDT */

.section .rodata
.align 8
gdt:
    .quad 0x0000000000000000            /* 0x00 null                         */
    .quad 0x00AF9A000000FFFF            /* 0x08 code64: L=1, DPL0, present   */
    .quad 0x00CF92000000FFFF            /* 0x10 data64: RW, present          */
    .quad 0x00CF9A000000FFFF            /* 0x18 code32: G=1, D=1, present    */
    .quad 0x00CF92000000FFFF            /* 0x20 data32: G=1, D=1, present    */
gdt_end:

.align 8
gdt_ptr:
    .word gdt_end - gdt - 1             /* limit                             */
    .quad gdt                           /* base (low 4 bytes used in 32-bit) */

/* ------------------------------------------------------------- .bss and stack */

.section .bss
.align 8
.global mb_magic
mb_magic:
    .long 0
.global mb_info
mb_info:
    .long 0

.align 4096
.global boot_pml4
boot_pml4:
pml4:
    .space 4096
.align 4096
.global boot_pdpt
boot_pdpt:
pdpt:
    .space 4096
.align 4096
.global boot_pd
boot_pd:
pd:
    .space 4096

.section .stack, "aw", @nobits
.align 16
stack_bottom:
    .space 65536
.global stack_top
stack_top:

/* Mark the stack as non-executable for the linker. */
.section .note.GNU-stack, "", @progbits
