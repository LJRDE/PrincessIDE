/* PrincessIDE reference kernel — IDT construction.
 *
 * Only the architectural exception vectors 0..31 are installed; everything
 * else points at the #UD stub so that a stray hardware interrupt is reported
 * rather than triple-faulting the machine.
 */
#include <stdint.h>

#include "idt.h"

#define IDT_ENTRIES      256
#define GDT_SELECTOR_CS  0x08   /* GDT_CODE64 from boot.S */

#define IDT_GATE_INTERRUPT 0x8E /* present, DPL=0, 64-bit interrupt gate */

struct idt_entry {
    uint16_t offset_low;
    uint16_t selector;
    uint8_t  ist;
    uint8_t  type_attr;
    uint16_t offset_mid;
    uint32_t offset_high;
    uint32_t zero;
} __attribute__((packed));

struct idt_pointer {
    uint16_t limit;
    uint64_t base;
} __attribute__((packed));

static struct idt_entry idt[IDT_ENTRIES];
static struct idt_pointer idtr;

/* Emitted by isr.S. */
extern void (*const isr_stub_table[32])(void);

static void set_gate(unsigned vector, uint64_t handler)
{
    idt[vector].offset_low  = (uint16_t)(handler & 0xFFFFu);
    idt[vector].selector    = GDT_SELECTOR_CS;
    idt[vector].ist         = 0;
    idt[vector].type_attr   = IDT_GATE_INTERRUPT;
    idt[vector].offset_mid  = (uint16_t)((handler >> 16) & 0xFFFFu);
    idt[vector].offset_high = (uint32_t)((handler >> 32) & 0xFFFFFFFFu);
    idt[vector].zero        = 0;
}

void idt_init(void)
{
    unsigned i;

    /* Default every vector to the invalid-opcode stub. */
    for (i = 0; i < IDT_ENTRIES; i++) {
        set_gate(i, (uint64_t)isr_stub_table[6]);
    }
    /* Dedicated stubs for the architectural exceptions. */
    for (i = 0; i < 32; i++) {
        set_gate(i, (uint64_t)isr_stub_table[i]);
    }

    idtr.limit = (uint16_t)(sizeof(idt) - 1);
    idtr.base  = (uint64_t)(uintptr_t)&idt;

    __asm__ volatile("lidt %0" : : "m"(idtr));
}
