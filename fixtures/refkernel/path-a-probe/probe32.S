/* PrincessIDE P0 — boot path A experiment (NOT part of the reference kernel).
 *
 * Purpose: show why `qemu-system-x86_64 -kernel` cannot be used to boot the
 * x86_64 reference kernel.
 *
 * Finding (reproducible):
 *   qemu-system-x86_64 ... -kernel fixtures/refkernel/build/refkernel.elf
 *     -> qemu-system-x86_64: Cannot load x86-64 image, give a 32bit one.
 *   QEMU's multiboot option ROM only accepts ELF32 images.
 *
 * This tiny 32-bit Multiboot 1 kernel proves the -kernel path itself works on
 * this host, so the rejection is purely about ELF class:
 *
 *   gcc -m32 -c probe32.S -o probe32.o
 *   ld -m elf_i386 -T probe32.ld -nostdlib -o probe32.elf probe32.o
 *   qemu-system-x86_64 -L "$PRINCESSIDE_QEMU_DATA" -m 128M -kernel probe32.elf \
 *       -display none -serial stdio -no-reboot
 *     -> PATH-A-32BIT-MULTIBOOT-BOOTED
 *
 * Boot path A was therefore rejected: it works, but forces a 32-bit kernel,
 * while this fixture must be a genuine x86_64 (long mode) kernel.
 */
.set MB_MAGIC, 0x1BADB002
.set MB_FLAGS, 0x00000003

.set COM1_DATA, 0x3F8
.set COM1_LSR,  0x3FD

.section .multiboot, "a"
.align 4
    .long MB_MAGIC
    .long MB_FLAGS
    .long -(MB_MAGIC + MB_FLAGS)

.section .text, "ax"
.code32
.global _start
_start:
    leal    msg, %esi
.Lnext_char:
    movb    (%esi), %al
    testb   %al, %al
    jz      .Ldone
.Lwait_tx:
    movl    $COM1_LSR, %edx
    inb     %dx, %al
    testb   $0x20, %al              /* LSR.THRE: transmit holding reg empty */
    jz      .Lwait_tx
    movb    (%esi), %al
    movl    $COM1_DATA, %edx
    outb    %al, %dx
    incl    %esi
    jmp     .Lnext_char
.Ldone:
    cli
    hlt
    jmp     .Ldone

.section .rodata
msg:
    .asciz "PATH-A-32BIT-MULTIBOOT-BOOTED\n"

.section .note.GNU-stack, "", @progbits
