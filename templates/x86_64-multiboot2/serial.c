/* PrincessIDE kernel project template — COM1 serial console.
 *
 * Freestanding 16550 UART driver in polled mode.  It talks to the I/O ports
 * directly; there is no libc and no interrupt controller involved.
 */
#include <stdint.h>

#include "serial.h"

#define COM1_BASE 0x3F8

#define UART_DATA (COM1_BASE + 0)   /* DLAB=0: receive/transmit buffer */
#define UART_IER  (COM1_BASE + 1)   /* DLAB=0: interrupt enable        */
#define UART_DLL  (COM1_BASE + 0)   /* DLAB=1: divisor low             */
#define UART_DLM  (COM1_BASE + 1)   /* DLAB=1: divisor high            */
#define UART_FCR  (COM1_BASE + 2)   /* FIFO control                    */
#define UART_LCR  (COM1_BASE + 3)   /* line control                    */
#define UART_MCR  (COM1_BASE + 4)   /* modem control                   */
#define UART_LSR  (COM1_BASE + 5)   /* line status                     */

#define LSR_THR_EMPTY 0x20u

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
    outb(UART_IER, 0x00);   /* no interrupts: we poll                     */
    outb(UART_LCR, 0x80);   /* DLAB on                                    */
    outb(UART_DLL, 0x01);   /* divisor 1 => 115200 baud                   */
    outb(UART_DLM, 0x00);
    outb(UART_LCR, 0x03);   /* 8 data bits, no parity, 1 stop; DLAB off   */
    outb(UART_FCR, 0xC7);   /* enable + clear FIFOs, 14-byte threshold    */
    outb(UART_MCR, 0x0B);   /* DTR | RTS | OUT2                           */
}

void serial_putc(char c)
{
    if (c == '\n') {
        serial_putc('\r');          /* terminals expect CRLF                  */
    }
    while ((inb(UART_LSR) & LSR_THR_EMPTY) == 0) {
        /* spin until the transmit holding register can accept a byte */
    }
    outb(UART_DATA, (uint8_t)c);
}

void serial_puts(const char *s)
{
    while (*s != '\0') {
        serial_putc(*s++);
    }
}

void serial_puthex32(uint32_t value)
{
    static const char digits[] = "0123456789abcdef";

    for (int shift = 28; shift >= 0; shift -= 4) {
        serial_putc(digits[(value >> shift) & 0xFu]);
    }
}
