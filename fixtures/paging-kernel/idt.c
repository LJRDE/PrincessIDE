/* PrincessIDE paging kernel — IDT construction.
 *
 * Every architectural exception vector 0..31 gets its own stub.  Vector 14
 * (#PF) is the one the fixture actually takes; vector 6 (#UD) is installed as
 * the catch-all so a stray hardware interrupt is reported rather than
 * triple-faulting the machine.
 *
 * Both the table and the IDTR are exported (non-static) so the fixture can dump
 * them on COM1 and the IDE's GDT/IDT descriptor view can parse a known-good
 * target.
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

/* Exported so paging.c / the IDE can walk the table. */
struct idt_entry pagingkernel_idt[IDT_ENTRIES];
struct idt_pointer pagingkernel_idtr;

/* Emitted by isr.S. */
extern void (*const isr_stub_table[32])(void);

static void set_gate(unsigned vector, uint64_t handler)
{
    pagingkernel_idt[vector].offset_low  = (uint16_t)(handler & 0xFFFFu);
    pagingkernel_idt[vector].selector    = GDT_SELECTOR_CS;
    pagingkernel_idt[vector].ist         = 0;
    pagingkernel_idt[vector].type_attr   = IDT_GATE_INTERRUPT;
    pagingkernel_idt[vector].offset_mid  = (uint16_t)((handler >> 16) & 0xFFFFu);
    pagingkernel_idt[vector].offset_high = (uint32_t)((handler >> 32) & 0xFFFFFFFFu);
    pagingkernel_idt[vector].zero        = 0;
}

void idt_init(void)
{
    unsigned i;

    /* Default every vector to the invalid-opcode stub. */
    for (i = 0; i < IDT_ENTRIES; i++) {
        set_gate(i, (uint64_t)isr_stub_table[6]);
    }
    /* Dedicated stubs for the architectural exceptions (#PF included). */
    for (i = 0; i < 32; i++) {
        set_gate(i, (uint64_t)isr_stub_table[i]);
    }

    pagingkernel_idtr.limit = (uint16_t)(sizeof(pagingkernel_idt) - 1);
    pagingkernel_idtr.base  = (uint64_t)(uintptr_t)&pagingkernel_idt;

    __asm__ volatile("lidt %0" : : "m"(pagingkernel_idtr));
}
