/* PrincessIDE paging kernel — page table walk, control registers, descriptors.
 *
 * boot.S builds the actual 4-level hierarchy; this header/c file is the
 * *reader*: it re-walks the tables from CR3, decodes x86_64 page-table entries
 * and prints the result on COM1.  That is exactly the operation P5-5 ("walk
 * the page tables from CR3 and parse each level") has to implement, so the
 * fixture is a known-good reference for it.
 */
#ifndef PAGINGKERNEL_PAGING_H
#define PAGINGKERNEL_PAGING_H

#include <stdint.h>

#define PAGE_SIZE        0x1000UL
#define HUGE_PAGE_SIZE   0x200000UL
#define IDENTITY_LIMIT   0x40000000UL      /* identity map covers 0 .. 1 GiB */

/* Deliberate holes built by boot.S.  The fault probe reads UNMAPPED_ADDR so
 * that a real #PF is raised with CR0.PG=1. */
#define UNMAPPED_ADDR      0x0000000000400000UL  /* PD[2] absent: 4 MiB window */
#define UNMAPPED_PAGE_ADDR 0x00000000001FF000UL  /* PT[511] absent: one 4 KiB page */

/* x86_64 page-table entry flag bits. */
#define PTE_PRESENT  (1UL << 0)
#define PTE_WRITE    (1UL << 1)
#define PTE_USER     (1UL << 2)

/* Tables built by boot.S, exported for walking/printing. */
extern uint64_t boot_pml4[512];
extern uint64_t boot_pdpt[512];
extern uint64_t boot_pd[512];
extern uint64_t boot_pt[512];

/* ----------------------------------------------------------- control regs -- */

static inline uint64_t read_cr0(void)
{
    uint64_t v;
    __asm__ volatile("mov %%cr0, %0" : "=r"(v));
    return v;
}

static inline uint64_t read_cr2(void)
{
    uint64_t v;
    __asm__ volatile("mov %%cr2, %0" : "=r"(v));
    return v;
}

static inline uint64_t read_cr3(void)
{
    uint64_t v;
    __asm__ volatile("mov %%cr3, %0" : "=r"(v));
    return v;
}

static inline uint64_t read_cr4(void)
{
    uint64_t v;
    __asm__ volatile("mov %%cr4, %0" : "=r"(v));
    return v;
}

/* Print CR0/CR2/CR3/CR4, walk PML4 -> PDPT -> PD -> PT from CR3 and print the
 * decoded entries plus the human-readable mapping summary. */
void paging_report(void);

#endif /* PAGINGKERNEL_PAGING_H */
