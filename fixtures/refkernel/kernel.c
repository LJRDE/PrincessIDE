/* PrincessIDE reference kernel — main.
 *
 * Behaviour required by the P0 test fixture contract:
 *   1. print the fixed banner "PrincessIDE reference kernel booted" on COM1,
 *   2. install an IDT,
 *   3. raise exactly one CPU exception from inside the stably named function
 *      refkernel_fault_probe(), and report the faulting RIP on COM1 in a
 *      machine-parsable form so scripts/smoke-boot.sh can assert on it and
 *      scripts/symbolicate.sh can map it back to a source line.
 */
#include <stdint.h>

#include "idt.h"
#include "serial.h"

extern uint32_t mb_magic;
extern uint32_t mb_info;

/* Multiboot magic values, one per header flavour. */
#define MULTIBOOT1_BOOT_MAGIC 0x2BADB002u
#define MULTIBOOT2_BOOT_MAGIC 0x36D76289u

/* A non-empty .data section matters for the image layout: with no initialised
 * data at all the kernel's RW PT_LOAD would be NOBITS-only and the linker gives
 * it file offset 0.  Printing this value also proves the RW segment was loaded
 * and its initialised contents are intact. */
uint64_t refkernel_image_magic = 0x1DEA0C0FFEE0BEEFULL;

static const char *vector_name(uint64_t vector)
{
    switch (vector) {
    case 0:  return "#DE divide error";
    case 1:  return "#DB debug";
    case 2:  return "NMI";
    case 3:  return "#BP breakpoint";
    case 4:  return "#OF overflow";
    case 5:  return "#BR bound range exceeded";
    case 6:  return "#UD invalid opcode";
    case 7:  return "#NM device not available";
    case 8:  return "#DF double fault";
    case 10: return "#TS invalid TSS";
    case 11: return "#NP segment not present";
    case 12: return "#SS stack-segment fault";
    case 13: return "#GP general protection";
    case 14: return "#PF page fault";
    case 16: return "#MF x87 floating-point";
    case 17: return "#AC alignment check";
    case 18: return "#MC machine check";
    case 19: return "#XM SIMD floating-point";
    case 21: return "#CP control protection";
    default: return "unknown exception";
    }
}

static void halt_forever(void) __attribute__((noreturn));

static void halt_forever(void)
{
    for (;;) {
        __asm__ volatile("cli; hlt");
    }
}

/* --------------------------------------------------------------------------
 * Panic path.
 *
 * Output format is a contract with the P0 test scripts:
 *   scripts/smoke-boot.sh  greps for  "EXCEPTION: vector=" and "FAULT_RIP="
 *   scripts/symbolicate.sh greps "FAULT_RIP=0x..." and feeds it to addr2line.
 * Do not reword these lines without updating those scripts.
 * -------------------------------------------------------------------------- */
void exception_dispatch(struct exception_frame *frame)
{
    serial_printf("\n");
    serial_printf("[refkernel] EXCEPTION: vector=0x%02lx (%s)\n",
                  (unsigned long)frame->vector, vector_name(frame->vector));
    serial_printf("[refkernel] FAULT_RIP=0x%016lx cs=0x%04lx rflags=0x%016lx error=0x%016lx\n",
                  (unsigned long)frame->rip,
                  (unsigned long)frame->cs,
                  (unsigned long)frame->rflags,
                  (unsigned long)frame->error);
    serial_printf("[refkernel] PANIC: unhandled CPU exception, halting\n");
    halt_forever();
}

/* --------------------------------------------------------------------------
 * The controlled-fault probe.
 *
 * The faulting instruction must stay inside this function and the function must
 * keep its name and stay out of line: fixtures/refkernel/tests (and the P0
 * symbolication test) assert that the reported RIP resolves to *this* function.
 *
 * `ud2` is used rather than a divide-by-zero because it is deterministic: it is
 * never folded away, never depends on optimisation level, and always raises #UD
 * (vector 6) with an error code of 0.
 * -------------------------------------------------------------------------- */
__attribute__((noinline, used))
void refkernel_fault_probe(void)
{
    __asm__ volatile("ud2" : : : "memory");

    /* Not reached: #UD is delivered before the next instruction retires. */
    serial_puts("[refkernel] BUG: fault probe returned\n");
    halt_forever();
}

/* ------------------------------------------------------------------------ */

void kernel_main(uint32_t magic, uint32_t info)
{
    serial_init();

    /* --- the fixed banner the smoke test asserts on ------------------------ */
    serial_puts("PrincessIDE reference kernel booted\n");

    serial_printf("[refkernel] multiboot: magic=0x%08x info=0x%08x (%s)\n",
                  (unsigned)magic, (unsigned)info,
                  magic == MULTIBOOT1_BOOT_MAGIC ? "multiboot 1" :
                  magic == MULTIBOOT2_BOOT_MAGIC ? "multiboot 2" : "unrecognised");
    serial_printf("[refkernel] cpu: long mode active, identity map 0x0..0x40000000\n");
    serial_printf("[refkernel] build: " __DATE__ " " __TIME__ "\n");
    serial_printf("[refkernel] data segment image magic=0x%016lx\n",
                  (unsigned long)refkernel_image_magic);

    idt_init();
    serial_printf("[refkernel] idt installed, raising controlled fault in refkernel_fault_probe()\n");

    refkernel_fault_probe();

    serial_puts("[refkernel] BUG: control returned from refkernel_fault_probe()\n");
    halt_forever();
}
