/* PrincessIDE paging kernel — boot entry and 4-level page table construction.
 *
 * Boot path: Multiboot2 (GRUB ISO, `qemu -cdrom -boot d`).  The loader hands the
 * image over in 32-bit protected mode with paging off: EAX = boot magic,
 * EBX = address of the Multiboot information structure.  This file:
 *
 *   1. stashes the Multiboot handoff registers,
 *   2. installs a flat GDT and loads it with `lgdt`,
 *   3. zeroes and fills a full 4-level table hierarchy PML4 -> PDPT -> PD -> PT:
 *        PML4[0]  -> PDPT                     (covers 0 .. 512 GiB)
 *        PDPT[0]  -> PD                       (covers 0 .. 1 GiB)
 *        PD[0]    -> PT with 4 KiB pages      (0x000000 .. 0x1FFFFF)
 *        PD[1]     = 2 MiB huge page          (0x200000 .. 0x3FFFFF)
 *        PD[2]     = not present              (0x400000 .. 0x5FFFFF)  <-- hole
 *        PD[3..511]= 2 MiB huge pages         (0x600000 .. 0x3FFFFFFF)
 *        PT[0..510]= 4 KiB pages              (0x000000 .. 0x1FEFFF)
 *        PT[511]   = not present              (0x1FF000 .. 0x1FFFFF)  <-- hole
 *      Both holes are deliberate: the kernel later dereferences 0x400000 to
 *      raise a *real* #PF (page fault) with paging actually enabled.
 *   4. enables PAE + LME + CR0.PG and far-jumps into 64-bit long mode,
 *   5. calls kernel_main().
 *
 * Freestanding: no libc, no third party code.
 *
 * NOTE (contract §4 / decision D3): the Multiboot2 header must sit inside the
 * first 32 KiB of the *file*, so .multiboot/.multiboot2 are placed first in
 * the linker script and this file.
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
    /* Multiboot 1 header — kept so the image is also recognisable by tools that
     * only understand Multiboot 1.  The boot path used here (D2) is Multiboot2
     * via GRUB, never `qemu -kernel`. */
    .long 0x1BADB002
    .long 0x00000003
    .long -(0x1BADB002 + 0x00000003)

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
    .short 0
    .long 16
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

    /* Normalise the segments onto our own flat 32-bit descriptors. */
    lgdt    pagingkernel_gdtr
    ljmp    $GDT_CODE32, $.Lflat32
.Lflat32:
    movw    $GDT_DATA32, %ax
    movw    %ax, %ds
    movw    %ax, %es
    movw    %ax, %ss
    movw    %ax, %fs
    movw    %ax, %gs

    /* ------------------------------------------------ 4-level page tables ---- */

    /* Zero all four tables explicitly.  Never rely on the loader to zero a
     * NOBITS section for us: a stale entry would silently fabricate a mapping. */
    leal    pml4, %edi
    xorl    %eax, %eax
    movl    $(4 * 4096 / 4), %ecx       /* 4096 dwords = four 4 KiB tables */
    rep stosl

    /* PML4[0] -> PDPT (present + writable). */
    movl    $pdpt, %eax
    orl     $0x3, %eax
    movl    %eax, pml4
    movl    $0, pml4 + 4

    /* PDPT[0] -> PD (present + writable). */
    movl    $pd, %eax
    orl     $0x3, %eax
    movl    %eax, pdpt
    movl    $0, pdpt + 4

    /* PD[0] -> PT: the first 2 MiB are mapped with 4 KiB pages, so the walk
     * PML4 -> PDPT -> PD -> PT is exercised by the IDE. */
    movl    $pt, %eax
    orl     $0x3, %eax
    movl    %eax, pd
    movl    $0, pd + 4

    /* PD[1] = 0x200000 | P | RW | PS : 2 MiB huge page (0x200000..0x3FFFFF). */
    movl    $0x200000, %eax
    orl     $0x83, %eax
    movl    %eax, pd + 8
    movl    $0, pd + 12

    /* PD[2] = 0 : deliberately NOT PRESENT (0x400000..0x5FFFFF).  This is the
     * address the fault probe reads to generate #PF. */
    movl    $0, pd + 16
    movl    $0, pd + 20

    /* PD[3..511] = identity-mapped 2 MiB huge pages up to 1 GiB. */
    movl    $3, %ecx
.Lfill_pd:
    movl    %ecx, %eax
    shll    $21, %eax                   /* index * 0x200000 */
    orl     $0x83, %eax                 /* P | RW | PS (2 MiB) */
    movl    %eax, pd(,%ecx,8)
    movl    $0, pd + 4(,%ecx,8)
    incl    %ecx
    cmpl    $512, %ecx
    jb      .Lfill_pd

    /* PT[0..510] = identity-mapped 4 KiB pages (0x000000..0x1FEFFF). */
    xorl    %ecx, %ecx
.Lfill_pt:
    movl    %ecx, %eax
    shll    $12, %eax                   /* index * 0x1000 */
    orl     $0x3, %eax                  /* P | RW (4 KiB) */
    movl    %eax, pt(,%ecx,8)
    movl    $0, pt + 4(,%ecx,8)
    incl    %ecx
    cmpl    $511, %ecx
    jb      .Lfill_pt
    /* PT[511] stays 0: second deliberate hole at 0x1FF000. */

    /* -------------------------------------------------------- long mode ---- */
    movl    %cr4, %eax
    orl     $0x20, %eax                 /* CR4.PAE */
    movl    %eax, %cr4

    movl    $pml4, %eax                 /* identity map => physical address */
    movl    %eax, %cr3

    movl    $0xC0000080, %ecx           /* IA32_EFER */
    rdmsr
    orl     $0x100, %eax                /* EFER.LME */
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
.align 16
.global pagingkernel_gdt
pagingkernel_gdt:
    .quad 0x0000000000000000            /* 0x00 null                          */
    .quad 0x00AF9A000000FFFF            /* 0x08 code64: L=1, DPL0, present    */
    .quad 0x00CF92000000FFFF            /* 0x10 data64: RW, present           */
    .quad 0x00CF9A000000FFFF            /* 0x18 code32: G=1, D=1, present     */
    .quad 0x00CF92000000FFFF            /* 0x20 data32: G=1, D=1, present     */
pagingkernel_gdt_end:

.align 16
.global pagingkernel_gdtr
pagingkernel_gdtr:
    .word pagingkernel_gdt_end - pagingkernel_gdt - 1   /* limit             */
    .quad pagingkernel_gdt                              /* base (all 8 bytes) */

/* ------------------------------------------------------------- .bss and stack */

.section .bss
.align 8
.global mb_magic
mb_magic:
    .long 0
.global mb_info
mb_info:
    .long 0

/* The four page tables.  Each must be 4 KiB aligned: CR3 stores the PML4
 * physical address with its low 12 bits ignored, and every table is indexed
 * with a 12-bit offset.  They are exported so paging.c (and the IDE) can walk
 * them and print/parse every level. */
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
.align 4096
.global boot_pt
boot_pt:
pt:
    .space 4096

.section .stack, "aw", @nobits
.align 16
stack_bottom:
    .space 65536
.global stack_top
stack_top:

/* Mark the stack as non-executable for the linker. */
.section .note.GNU-stack, "", @progbits
