# PrincessIDE paging kernel — P4/P5 test fixture

A freestanding, debug-info-carrying x86_64 kernel that **actually enables
paging** (CR0.PG = 1) with a full 4-level table hierarchy, installs a GDT and an
IDT, reports its live CR3 / page-table walk on COM1, and then deliberately
dereferences an unmapped address so a **real #PF** is delivered and reported.

This is the fixture P4 (graphical debugging: read CR3, walk the page tables) and
P5 (page-table / GDT / IDT visualisation, QEMU monitor parsing) can be accepted
against.  The pre-existing `fixtures/refkernel/` fixture is left untouched: it
only raises `#UD` and — per research report B §3.2 — is not usable for paging
assertions.

## Why a separate fixture

`#PF` is impossible while `CR0.PG = 0`: an access to an invalid address is just
a physical read.  Research report B §3.2 documents the trap ("分页没开就断言
#PF").  This fixture turns paging on for real and proves it before faulting, so
the fault path is a genuine page fault with a valid CR2.

`fixtures/refkernel/` is left untouched (decision D3) and stays the fixture for
the #UD / symbolication / boot smoke path.  Its `boot.S` does build a 1 GiB
identity map out of 2 MiB huge pages and does enter long mode, but it has **no
4-level PT walk to exercise, no deliberately unmapped address, no `#PF` path,
and it never reports CR3** — none of which P4/P5's paging features can be
accepted against.  That gap is what this fixture fills.

## Boot path (decisions D2/D9)

* x86_64 + **Multiboot2** + GRUB ISO, booted with `qemu -cdrom ... -boot d`.
  Never `-kernel` (QEMU's multiboot option ROM only accepts ELF32).
* Headless serial contract: `-display none -serial stdio -monitor none`.
* No `-nographic`.

## The memory map it builds

`boot.S` zeroes and fills four 4 KiB-aligned tables (`boot_pml4`, `boot_pdpt`,
`boot_pd`, `boot_pt`) before turning on PAE + LME + paging:

| Range | Mapping | Where |
|---|---|---|
| `0x0000000000000000` – `0x00000000001FEFFF` | 4 KiB pages, present, RW | `PT[0..510]` via `PD[0]` |
| `0x00000000001FF000` – `0x00000000001FFFFF` | **not present** (deliberate hole) | `PT[511] = 0` |
| `0x0000000000200000` – `0x00000000003FFFFF` | 2 MiB huge page, present, RW | `PD[1]` |
| `0x0000000000400000` – `0x00000000005FFFFF` | **not present** (deliberate hole, the fault target) | `PD[2] = 0` |
| `0x0000000000600000` – `0x000000003FFFFFFF` | 2 MiB huge pages, present, RW | `PD[3..511]` |

So the hierarchy exercises every level: `PML4[0] -> PDPT[0] -> PD[0] -> PT[i]`
(4 KiB), plus huge-page leaves at the PD level, plus two holes.  `info mem`
shows exactly these ranges (see `fixtures/qemu-monitor/`).

`paging_report()` (in `paging.c`) re-walks the tables from the live **CR3** and
prints each entry decoded (`phys`, `P`, `RW`, `US`), then `sgdt`/`sidt` and the
GDT descriptors and the parsed `IDT[14]` gate.  That is precisely the P5-5
operation, implemented inside the guest as a reference.

## Output contract (machine-parsable)

The fixture prints, on COM1:

```
PrincessIDE paging kernel booted              <- fixed banner (P2)
[pagingkernel] CR0=... CR3=... CR4=...
[pagingkernel] CR0.PG=1 ...
[pagingkernel] PAGING_ENABLED cr3=0x... cr0=0x...   <- paging-on evidence (P3)
[pagingkernel] EXCEPTION: vector=0x0e (#PF page fault)
[pagingkernel] FAULT_RIP=0x...................       <- symbolicate with addr2line (P6)
[pagingkernel] FAULT_ERROR=0x...
[pagingkernel] FAULT_ADDR=0x...                      <- CR2
[pagingkernel] PANIC: unhandled CPU exception, halting
```

The banner is intentionally different from the reference kernel's
(`PrincessIDE reference kernel booted`) so the two fixtures can never satisfy
each other's assertions.

Faulting symbol: **`paging_fault_probe`** (`kernel.c`), a `noinline` function
whose single volatile load is the faulting instruction; the fault RIP resolves
to `kernel.c` with `addr2line -f -C`.

## Build and run

```bash
source /root/PrincessIDE/scripts/env.sh
make -C fixtures/paging-kernel -j2       # build/pagingkernel.elf (ELF64, -g, symbols)
make -C fixtures/paging-kernel -j2 iso   # build/pagingkernel.iso  (GRUB Multiboot2)
make -C fixtures/paging-kernel run       # headless boot; asserts banner + #PF
make -C fixtures/paging-kernel clean
```

`make run` sources `scripts/env.sh` itself (via `run.sh`) and needs no
arguments.  Keep `-j2` at most (host has no swap, D16).

## Provenance / how to inspect further

* `princess.toml` — project descriptor, field names per contracts §4,
  `boot = "multiboot2"`.
* `fixtures/qemu-monitor/` — recorded `info registers` / `info mem` /
  `info tlb` / `info cpus` samples taken while this kernel is paging-enabled
  and halted in its #PF handler, plus `regenerate.sh` to re-record them.

## Files

| File | Role |
|---|---|
| `boot.S` | Multiboot headers, GDT, 4-level page tables, long-mode entry |
| `isr.S` | exception entry stubs for vectors 0..31 |
| `idt.c` / `idt.h` | IDT construction (#PF and #UD gates) + exported IDTR |
| `paging.c` / `paging.h` | CR0/CR2/CR3/CR4 readers, CR3 table walk, GDT/IDT dump |
| `serial.c` / `serial.h` | polled COM1 console and mini `printf` |
| `kernel.c` | `kernel_main`, `exception_dispatch`, `paging_fault_probe` |
| `linker.ld` | 1 MiB load address, RX + RW segments |
| `grub.cfg` | GRUB Multiboot2 menu |
| `Makefile` | `all` / `iso` / `run` / `symbols` / `check` / `clean` |
| `run.sh` | headless runner + assertion on banner and #PF report |
| `princess.toml` | contract-style project descriptor |
