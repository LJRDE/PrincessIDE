/* PrincessIDE reference kernel — IDT and CPU exception dispatch. */
#ifndef REFKERNEL_IDT_H
#define REFKERNEL_IDT_H

#include <stdint.h>

/* Register/exception frame pushed by the stubs in isr.S.
 *
 * Field order is significant: it mirrors the push order in isr_common, whose
 * last push (rax) therefore sits at the lowest address, i.e. offset 0 of this
 * structure (which is addressed through RSP). */
struct exception_frame {
    uint64_t rax, rbx, rcx, rdx, rsi, rdi, rbp;
    uint64_t r8, r9, r10, r11, r12, r13, r14, r15;
    uint64_t vector;    /* exception vector number                 */
    uint64_t error;     /* error code, or 0 when the CPU pushes none */
    uint64_t rip;       /* instruction pointer at the fault        */
    uint64_t cs;
    uint64_t rflags;
} __attribute__((packed));

/* Install the IDT with stubs for vectors 0..31. */
void idt_init(void);

/* Implemented in kernel.c; called from isr.S and never returns. */
void exception_dispatch(struct exception_frame *frame) __attribute__((noreturn));

#endif /* REFKERNEL_IDT_H */
