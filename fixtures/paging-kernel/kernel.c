/* PrincessIDE paging kernel — main.
 *
 * Behaviour required by the P5 fixture contract:
 *   1. print the fixed banner "PrincessIDE paging kernel booted" on COM1
 *      (deliberately different from the reference kernel's banner, so the two
 *      fixtures can never impersonate one another),
 *   2. install an IDT that has dedicated #PF and #UD gates,
 *   3. report the live CR0/CR3 and walk the 4-level page tables (P5-5),
 *   4. dereference a deliberately unmapped address and take a *real* #PF,
 *      reporting error code / CR2 / fault RIP in machine-parsable form.
 *
 * #PF is only meaningful here because CR0.PG is genuinely 1 (boot.S turned it
 * on); the state is printed before the fault and repeated by the handler.
 */
#include <stdint.h>

#include "idt.h"
#include "paging.h"
#include "serial.h"

extern uint32_t mb_magic;
extern uint32_t mb_info;

/* Multiboot magic values, one per header flavour. */
#define MULTIBOOT1_BOOT_MAGIC 0x2BADB002u
#define MULTIBOOT2_BOOT_MAGIC 0x36D76289u

/* A non-empty .data section proves the RW PT_LOAD was loaded intact. */
uint64_t pagingkernel_image_magic = 0x9051DEADBEEFCAFEULL;

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
 * Output format is a contract with the P5 acceptance scripts / IDE parser:
 *   - "EXCEPTION: vector=0x0e (#PF page fault)"
 *   - "FAULT_RIP=0x................"  (symbolicate with addr2line)
 *   - "FAULT_ADDR=0x................" (the CR2 data address)
 *   - "FAULT_ERROR=0x................"
 * Do not reword these lines without updating the report/scripts.
 * -------------------------------------------------------------------------- */
void exception_dispatch(struct exception_frame *frame)
{
    serial_puts("\n");
    serial_printf("[pagingkernel] EXCEPTION: vector=0x%02lx (%s)\n",
                  (unsigned long)frame->vector, vector_name(frame->vector));
    serial_printf("[pagingkernel] FAULT_RIP=0x%016lx cs=0x%04lx rflags=0x%016lx\n",
                  (unsigned long)frame->rip,
                  (unsigned long)frame->cs,
                  (unsigned long)frame->rflags);
    serial_printf("[pagingkernel] FAULT_ERROR=0x%016lx\n",
                  (unsigned long)frame->error);

    if (frame->vector == 14) {
        /* #PF carries the faulting data address in CR2 and a decoded error
         * code: bit0 P (0 = not present, 1 = protection violation),
         * bit1 W/R, bit2 U/S, bit3 RSVD, bit4 I/D. */
        uint64_t cr2 = read_cr2();
        uint64_t e   = frame->error;
        serial_printf("[pagingkernel] FAULT_ADDR=0x%016lx\n", (unsigned long)cr2);
        serial_printf("[pagingkernel] PAGE_FAULT cause=%s access=%s ring=%s rsvd=%lu instr_fetch=%lu\n",
                      (e & 1) ? "protection-violation" : "not-present",
                      (e & 2) ? "write" : "read",
                      (e & 4) ? "user" : "supervisor",
                      (unsigned long)((e >> 3) & 1),
                      (unsigned long)((e >> 4) & 1));
        serial_printf("[pagingkernel] PAGE_FAULT cr3=0x%016lx cr0=0x%016lx cr4=0x%016lx\n",
                      (unsigned long)read_cr3(),
                      (unsigned long)read_cr0(),
                      (unsigned long)read_cr4());
    }

    serial_printf("[pagingkernel] PANIC: unhandled CPU exception, halting\n");
    halt_forever();
}

/* --------------------------------------------------------------------------
 * The controlled-fault probe.
 *
 * Reads the deliberately absent 4 MiB address.  The faulting load stays inside
 * this stably named function (noinline + volatile, -O0), so the reported RIP
 * resolves to this source line with addr2line.
 * -------------------------------------------------------------------------- */
__attribute__((noinline, used))
void paging_fault_probe(void)
{
    volatile uint64_t *probe = (volatile uint64_t *)(uintptr_t)UNMAPPED_ADDR;
    uint64_t value = *probe;            /* <-- #PF is delivered here */

    /* Not reached: a not-present page faults before the load retires. */
    serial_printf("[pagingkernel] BUG: read from 0x%016lx returned 0x%016lx\n",
                  (unsigned long)UNMAPPED_ADDR, (unsigned long)value);
    halt_forever();
}

/* ------------------------------------------------------------------------ */

void kernel_main(uint32_t magic, uint32_t info)
{
    serial_init();

    /* --- the fixed banner the smoke test asserts on ------------------------ */
    serial_puts("PrincessIDE paging kernel booted\n");

    serial_printf("[pagingkernel] multiboot: magic=0x%08x info=0x%08x (%s)\n",
                  (unsigned)magic, (unsigned)info,
                  magic == MULTIBOOT1_BOOT_MAGIC ? "multiboot 1" :
                  magic == MULTIBOOT2_BOOT_MAGIC ? "multiboot 2" : "unrecognised");
    serial_printf("[pagingkernel] build: " __DATE__ " " __TIME__ "\n");
    serial_printf("[pagingkernel] data segment image magic=0x%016lx\n",
                  (unsigned long)pagingkernel_image_magic);

    idt_init();
    serial_printf("[pagingkernel] IDT installed at 0x%016lx limit=0x%04x (#UD + #PF gates ready)\n",
                  (unsigned long)pagingkernel_idtr.base,
                  (unsigned)pagingkernel_idtr.limit);

    /* Report the live control registers and walk the tables built by boot.S. */
    paging_report();

    serial_printf("[pagingkernel] dereferencing deliberately unmapped address 0x%016lx ...\n",
                  (unsigned long)UNMAPPED_ADDR);
    paging_fault_probe();

    serial_puts("[pagingkernel] BUG: control returned from paging_fault_probe()\n");
    halt_forever();
}
