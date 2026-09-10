/* PrincessIDE paging kernel — COM1 serial console (16550 UART, polled).
 *
 * Freestanding: talks to the UART I/O ports directly, no libc.  This is the
 * only output channel of the fixture; decision D9 fixes the QEMU side to
 * `-display none -serial stdio -monitor none`.
 */
#ifndef PAGINGKERNEL_SERIAL_H
#define PAGINGKERNEL_SERIAL_H

/* Initialise COM1 (0x3F8) for 115200 8N1, polled output. */
void serial_init(void);

void serial_putc(char c);
void serial_puts(const char *s);

/* Minimal freestanding formatter.
 * Supported: %s %c %d %i %u %x %X %p, plus a leading 0 and/or a decimal field
 * width, e.g. %02x or %016lx.  The 'l' / 'll' length modifier selects a 64-bit
 * argument, exactly as in standard printf.  %p always emits 0x + 16 hex digits. */
void serial_printf(const char *fmt, ...);

#endif /* PAGINGKERNEL_SERIAL_H */
