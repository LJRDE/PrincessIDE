/* PrincessIDE kernel project template — kernel entry point.
 *
 * This is the file to start editing.  It prints the template banner on COM1 and
 * then halts.  There is deliberately no paging, scheduling, interrupt or memory
 * management here: add those as your kernel grows.
 *
 * Contract with templates/verify-template.sh:
 *   the exact banner line "PrincessIDE template kernel booted" must be printed
 *   on COM1.  It is intentionally different from the reference-kernel fixture
 *   banner ("PrincessIDE reference kernel booted") so the two can never be
 *   mistaken for one another.
 */
#include <stdint.h>

#include "serial.h"

#define MULTIBOOT2_BOOT_MAGIC 0x36D76289u

/* A non-empty initialised .data section is useful: it gives the RW PT_LOAD real
 * contents, and printing it back proves the loader copied that segment in. */
uint64_t template_image_magic = 0x7E3F00DBEEF1234ULL;

static void halt_forever(void) __attribute__((noreturn));

static void halt_forever(void)
{
    for (;;) {
        __asm__ volatile("cli; hlt");
    }
}

void kernel_main(uint32_t magic, uint32_t info)
{
    serial_init();

    /* --- the fixed banner templates/verify-template.sh asserts on --------- */
    serial_puts("PrincessIDE template kernel booted\n");

    serial_puts("[template] x86_64 long mode reached via Multiboot2\n");
    serial_puts("[template] handoff magic=0x");
    serial_puthex32(magic);
    serial_puts(" info=0x");
    serial_puthex32(info);
    serial_puts(magic == MULTIBOOT2_BOOT_MAGIC ? " (multiboot2)\n"
                                               : " (unexpected magic)\n");

    serial_puts("[template] image magic=0x");
    serial_puthex32((uint32_t)(template_image_magic >> 32));
    serial_puthex32((uint32_t)template_image_magic);
    serial_puts("\n");

    serial_puts("[template] COM1 polled serial ready; halting.\n");
    halt_forever();
}
