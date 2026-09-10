/* PrincessIDE reference kernel — COM1 serial console (16550 UART, polled).
 *
 * Freestanding: talks to the UART I/O ports directly, no libc.
 */
#include <stdarg.h>
#include <stdint.h>

#include "serial.h"

#define COM1_BASE 0x3F8

#define UART_DATA        (COM1_BASE + 0)   /* DLAB=0: RX/TX buffer          */
#define UART_IER         (COM1_BASE + 1)   /* DLAB=0: interrupt enable      */
#define UART_DLL         (COM1_BASE + 0)   /* DLAB=1: divisor low           */
#define UART_DLM         (COM1_BASE + 1)   /* DLAB=1: divisor high          */
#define UART_FCR         (COM1_BASE + 2)   /* FIFO control                  */
#define UART_LCR         (COM1_BASE + 3)   /* line control                  */
#define UART_MCR         (COM1_BASE + 4)   /* modem control                 */
#define UART_LSR         (COM1_BASE + 5)   /* line status                   */

#define LSR_THR_EMPTY    0x20u

static inline void outb(uint16_t port, uint8_t value)
{
    __asm__ volatile("outb %0, %1" : : "a"(value), "Nd"(port));
}

static inline uint8_t inb(uint16_t port)
{
    uint8_t value;
    __asm__ volatile("inb %1, %0" : "=a"(value) : "Nd"(port));
    return value;
}

void serial_init(void)
{
    outb(UART_IER, 0x00);   /* no interrupts: we poll */
    outb(UART_LCR, 0x80);   /* DLAB on                */
    outb(UART_DLL, 0x01);   /* divisor 1 => 115200    */
    outb(UART_DLM, 0x00);
    outb(UART_LCR, 0x03);   /* 8 data bits, no parity, 1 stop; DLAB off */
    outb(UART_FCR, 0xC7);   /* enable + clear FIFOs, 14-byte threshold   */
    outb(UART_MCR, 0x0B);   /* DTR | RTS | OUT2                          */
}

void serial_putc(char c)
{
    if (c == '\n') {
        serial_putc('\r');
    }
    while ((inb(UART_LSR) & LSR_THR_EMPTY) == 0) {
        /* spin until the transmit holding register drains */
    }
    outb(UART_DATA, (uint8_t)c);
}

void serial_puts(const char *s)
{
    while (*s != '\0') {
        serial_putc(*s++);
    }
}

/* ------------------------------------------------------------------ numbers */

static void emit_uint(uint64_t value, unsigned base, int upper, int width,
                      int pad_zero, int prefix)
{
    static const char lower_digits[] = "0123456789abcdef";
    static const char upper_digits[] = "0123456789ABCDEF";
    const char *digits = upper ? upper_digits : lower_digits;

    char buf[24];
    int n = 0;

    if (value == 0) {
        buf[n++] = '0';
    } else {
        while (value != 0) {
            buf[n++] = digits[value % base];
            value /= base;
        }
    }

    int zeros = 0;
    if (pad_zero) {
        int total = n + (prefix ? 2 : 0);
        if (width > total) {
            zeros = width - total;
        }
    }
    while (zeros-- > 0) {
        serial_putc('0');
    }
    if (prefix) {
        serial_putc('0');
        serial_putc(upper ? 'X' : 'x');
    }
    while (n-- > 0) {
        serial_putc(buf[n]);
    }
}

void serial_printf(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);

    for (const char *p = fmt; *p != '\0'; p++) {
        if (*p != '%') {
            serial_putc(*p);
            continue;
        }

        p++;
        if (*p == '%') {
            serial_putc('%');
            continue;
        }

        int pad_zero = 0;
        int width = 0;
        if (*p == '0') {
            pad_zero = 1;
            p++;
        }
        while (*p >= '0' && *p <= '9') {
            width = width * 10 + (*p - '0');
            p++;
        }

        int is_long = 0;
        if (*p == 'l') {
            is_long = 1;
            p++;
            if (*p == 'l') {
                p++;
            }
        }

        switch (*p) {
        case 's': {
            const char *s = va_arg(ap, const char *);
            serial_puts(s != 0 ? s : "(null)");
            break;
        }
        case 'c':
            serial_putc((char)va_arg(ap, int));
            break;
        case 'd':
        case 'i': {
            long long v = is_long ? va_arg(ap, long long) : (long long)va_arg(ap, int);
            if (v < 0) {
                serial_putc('-');
                emit_uint((uint64_t)(-v), 10, 0, width, pad_zero, 0);
            } else {
                emit_uint((uint64_t)v, 10, 0, width, pad_zero, 0);
            }
            break;
        }
        case 'u': {
            uint64_t v = is_long ? va_arg(ap, unsigned long)
                                 : (uint64_t)va_arg(ap, unsigned int);
            emit_uint(v, 10, 0, width, pad_zero, 0);
            break;
        }
        case 'x':
        case 'X': {
            uint64_t v = is_long ? va_arg(ap, unsigned long)
                                 : (uint64_t)va_arg(ap, unsigned int);
            emit_uint(v, 16, *p == 'X', width, pad_zero, 0);
            break;
        }
        case 'p':
            /* 0x followed by 16 zero-padded hex digits. */
            emit_uint((uint64_t)(uintptr_t)va_arg(ap, void *), 16, 0, 16, 1, 1);
            break;
        case 'n':
        default:
            serial_putc('%');
            serial_putc(*p);
            break;
        }
    }

    va_end(ap);
}
