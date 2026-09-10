/* PrincessIDE kernel project template — COM1 serial console.
 *
 * A polled 16550 UART driver: no interrupts, no libc.  Enough for a banner and
 * a couple of diagnostic lines, which is what a fresh project needs.
 */
#ifndef TEMPLATE_SERIAL_H
#define TEMPLATE_SERIAL_H

#include <stdint.h>

/* Initialise COM1 (I/O port 0x3F8) for 115200 8N1, polling. */
void serial_init(void);

void serial_putc(char c);
void serial_puts(const char *s);

/* Print a 32-bit value as exactly 8 lowercase hex digits (no "0x" prefix). */
void serial_puthex32(uint32_t value);

#endif /* TEMPLATE_SERIAL_H */
