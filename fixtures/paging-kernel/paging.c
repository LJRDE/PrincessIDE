/* PrincessIDE paging kernel — CR3 walk and mapping summary.
 *
 * P5-5 requires the IDE to walk the page tables from CR3 and parse each level.
 * This file performs the same walk *inside the guest* and prints the decoded
 * result, so the fixture both proves the tables are well formed and gives the
 * IDE a reference output to compare against.
 */
#include <stdint.h>

#include "paging.h"
#include "serial.h"

#define ENTRY_ADDR_MASK 0x000FFFFFFFFFF000UL

/* Structure of a GDTR/IDTR operand, as loaded by `lgdt`/`lidt`. */
struct descriptor_register {
    uint16_t limit;
    uint64_t base;
} __attribute__((packed));

/* Print one line per level of the walk.  `label` names the table, `index` the
 * entry, `entry` its raw value.  The address bits are decoded from the entry
 * rather than assumed, which is the whole point of a page-table viewer. */
static void print_entry(const char *label, unsigned index, uint64_t entry)
{
    serial_printf("[pagingkernel] %s[%u] = 0x%016lx -> phys=0x%016lx P=%lu RW=%lu US=%lu\n",
                  label, index, entry,
                  entry & ENTRY_ADDR_MASK,
                  (entry & PTE_PRESENT) ? 1UL : 0UL,
                  (entry & PTE_WRITE) ? 1UL : 0UL,
                  (entry & PTE_USER) ? 1UL : 0UL);
}

static void report_descriptors(void)
{
    struct descriptor_register gdtr;
    struct descriptor_register idtr;

    __asm__ volatile("sgdt %0" : "=m"(gdtr));
    __asm__ volatile("sidt %0" : "=m"(idtr));

    serial_printf("[pagingkernel] GDTR base=0x%016lx limit=0x%04x (%u bytes)\n",
                  gdtr.base, (unsigned)gdtr.limit, (unsigned)gdtr.limit + 1);
    serial_printf("[pagingkernel] IDTR base=0x%016lx limit=0x%04x (%u bytes)\n",
                  idtr.base, (unsigned)idtr.limit, (unsigned)idtr.limit + 1);

    /* Dump the five GDT descriptors (null, code64, data64, code32, data32). */
    {
        const uint64_t *gdt = (const uint64_t *)(uintptr_t)gdtr.base;
        unsigned count = ((unsigned)gdtr.limit + 1) / 8;
        unsigned i;
        for (i = 0; i < count; i++) {
            serial_printf("[pagingkernel] GDT[%u]=0x%016lx\n", i, gdt[i]);
        }
    }

    /* Parse the #PF gate (vector 14) the way a descriptor view must:
     *   bytes 0-1 offset low, 2-3 selector, 4 IST, 5 type/attr,
     *   bytes 6-7 offset mid, 8-11 offset high, 12-15 reserved. */
    {
        const uint8_t *idt = (const uint8_t *)(uintptr_t)idtr.base;
        const uint8_t *gate = idt + 14 * 16;
        uint64_t handler = (uint64_t)gate[0]
                         | ((uint64_t)gate[1] << 8)
                         | ((uint64_t)gate[6] << 16)
                         | ((uint64_t)gate[7] << 24)
                         | ((uint64_t)gate[8] << 32)
                         | ((uint64_t)gate[9] << 40)
                         | ((uint64_t)gate[10] << 48)
                         | ((uint64_t)gate[11] << 56);
        unsigned selector = (unsigned)gate[2] | ((unsigned)gate[3] << 8);
        serial_printf("[pagingkernel] IDT[14] (#PF) handler=0x%016lx sel=0x%04x ist=%u type=0x%02x\n",
                      handler, selector, (unsigned)gate[4], (unsigned)gate[5]);
    }
}

void paging_report(void)
{
    uint64_t cr0 = read_cr0();
    uint64_t cr2 = read_cr2();
    uint64_t cr3 = read_cr3();
    uint64_t cr4 = read_cr4();

    serial_puts("[pagingkernel] ---- paging state ----\n");
    serial_printf("[pagingkernel] CR0=0x%016lx CR2=0x%016lx CR3=0x%016lx CR4=0x%016lx\n",
                  cr0, cr2, cr3, cr4);
    serial_printf("[pagingkernel] CR0.PG=%lu CR0.PE=%lu CR4.PAE=%lu\n",
                  (cr0 >> 31) & 1UL, cr0 & 1UL, (cr4 >> 5) & 1UL);

    /* Walk PML4 -> PDPT -> PD -> PT strictly from the live CR3 value.  The
     * identity mapping means these physical addresses are also dereferenceable
     * virtual addresses; if they were not, the walk itself would fault. */
    {
        const uint64_t *pml4 = (const uint64_t *)(uintptr_t)(cr3 & ENTRY_ADDR_MASK);
        uint64_t pml4e = pml4[0];
        const uint64_t *pdpt = (const uint64_t *)(uintptr_t)(pml4e & ENTRY_ADDR_MASK);
        uint64_t pdpte = pdpt[0];
        const uint64_t *pd = (const uint64_t *)(uintptr_t)(pdpte & ENTRY_ADDR_MASK);
        uint64_t pde0 = pd[0];
        const uint64_t *pt = (const uint64_t *)(uintptr_t)(pde0 & ENTRY_ADDR_MASK);

        serial_puts("[pagingkernel] ---- CR3 page-table walk ----\n");
        print_entry("PML4", 0, pml4e);
        print_entry("PDPT", 0, pdpte);
        print_entry("PD", 0, pde0);
        print_entry("PD", 1, pd[1]);   /* 2 MiB huge page                   */
        print_entry("PD", 2, pd[2]);   /* deliberately not present           */
        print_entry("PT", 0, pt[0]);
        print_entry("PT", 510, pt[510]);
        print_entry("PT", 511, pt[511]); /* deliberately not present         */

        serial_puts("[pagingkernel] ---- mapping summary ----\n");
        serial_puts("[pagingkernel] map 0x0000000000000000-0x00000000001fffff 4KiB pages RW (PT[0..510]), hole at 0x1ff000\n");
        serial_puts("[pagingkernel] map 0x0000000000200000-0x00000000003fffff 2MiB huge  RW (PD[1])\n");
        serial_puts("[pagingkernel] map 0x0000000000400000-0x00000000005fffff NOT PRESENT    (PD[2] deliberately absent)\n");
        serial_puts("[pagingkernel] map 0x0000000000600000-0x000000003fffffff 2MiB huge  RW (PD[3..511])\n");
    }

    report_descriptors();

    /* The next line is the machine-readable proof that paging is on: the
     * smoke/acceptance scripts grep for CR0.PG=1 together with a non-zero CR3. */
    serial_printf("[pagingkernel] PAGING_ENABLED cr3=0x%016lx cr0=0x%016lx\n", cr3, cr0);
    serial_printf("[pagingkernel] unmapped probe target: 0x%016lx (PD[2] absent)\n",
                  (unsigned long)UNMAPPED_ADDR);
}
