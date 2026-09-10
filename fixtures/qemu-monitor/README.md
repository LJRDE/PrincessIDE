# Recorded QEMU monitor samples (P5-4 fixture)

Verbatim monitor output recorded from a **real** QEMU run of
`fixtures/paging-kernel/`, taken **after the guest enabled paging** so that
`info mem` / `info tlb` are meaningful (while `CR0.PG=0` they are degenerate:
`info mem` prints `PG disabled`, `info tlb` prints nothing).

These files are the fixed fixtures the P5-4 monitor parser is asserted against.
They must not be hand-edited; re-record them with `regenerate.sh`.

## How to regenerate

```bash
bash fixtures/qemu-monitor/regenerate.sh
```

One command, non-interactive, cwd-independent: the script locates the repo from
its own path, sources `scripts/env.sh`, builds the paging-kernel ISO with
`make -j2`, boots QEMU, waits for the paging-enabled marker, and runs
`qmp_capture.py`.  Exit status is 0 only when every sample was written.

### The exact QEMU invocation

```bash
qemu-system-x86_64 \
  -L "$PRINCESSIDE_QEMU_DATA" -m 256M \
  -cdrom fixtures/paging-kernel/build/pagingkernel.iso -boot d \
  -display none -serial stdio -monitor none \
  -qmp unix:<tmp>/qmp.sock,server=on,wait=off \
  -no-reboot
```

* `-cdrom` + `-boot d` — GRUB Multiboot2 ISO (D2; never `-kernel`).
* `-display none -serial stdio -monitor none` — D9 serial contract; the machine
  interface is **QMP on a unix socket**, so stdio is never shared.
* No `-nographic`.

### Capture timing

1. QEMU starts; the guest boots, prints `PAGING_ENABLED cr3=0x... cr0=...`
   (`CR0.PG=1`), then deliberately dereferences the unmapped `0x400000` and
   takes a real `#PF`.
2. `regenerate.sh` polls the serial log until it contains **both
   `PAGING_ENABLED` and `PANIC`**.  At that point the CPU is halted (`hlt` with
   interrupts disabled) with CR3 still loaded, so the page tables the monitor
   walks are stable and exactly the ones under test.
3. `qmp_capture.py` connects to the QMP socket, sends `qmp_capabilities`, then
   issues `human-monitor-command` for each HMP query and the structured queries.

`query-status` returns `{"status": "running"}` even though the vCPU is halted:
that field describes the VM, not the CPU.  Use `info registers` (`HLT=1`) to
observe the halted vCPU.

### Environment used for the current samples

* QEMU: `QEMU emulator version 7.2.22 (Debian 1:7.2+dfsg-7+deb12u18+b3)`
  (`qemu-version.txt`)
* Guest: `fixtures/paging-kernel` ISO, 256 MiB RAM, one CPU, no KVM (TCG)
* Guest state at capture: `CR0=80000011`, `CR3=0000000000104000`,
  `CR2=0000000000400000`, `HLT=1` (`capture-serial.log`)

## Files

| File | Contents |
|---|---|
| `info-registers.txt` | HMP `info registers` — GPRs, RIP, segments, `CR0/CR2/CR3/CR4`, `EFER`, FPU/XMM |
| `info-mem.txt` | HMP `info mem` — the mapped virtual ranges (3 lines) |
| `info-tlb.txt` | HMP `info tlb` — one line per mapping, 1021 lines |
| `info-cpus.txt` | HMP `info cpus` — one vCPU line |
| `hmp-responses.jsonl` | the exact HMP `return` strings as sent over QMP (one JSON object per command) |
| `qmp-query-version.json` | structured `query-version` |
| `qmp-query-status.json` | structured `query-status` |
| `qmp-query-cpus-fast.json` | structured `query-cpus-fast` |
| `qmp-query-memory-size-summary.json` | structured `query-memory-size-summary` |
| `capture-serial.log` | guest serial at capture time (proves paging was on) |
| `capture-metadata.txt` | argv, timing, queries, provenance |
| `qemu-version.txt` | `qemu-system-x86_64 --version` |
| `qmp_capture.py` | the QMP client used (stdlib only) |
| `regenerate.sh` | one-shot regeneration |

## Format notes for the P5-4 parser

### Stable and safely parseable

* **`info mem`** (`<start>-<end> <size> <flags>`), one mapping per line, hex
  without `0x`.  Flags `-rw` for the ranges in this fixture.  Stable across
  7.x.
* **`info tlb`** (`<vaddr>: <paddr> <9 flag chars>`).  The two hex address
  columns are the primary parse target and are stable.  In QEMU 7.2 there is
  **no address argument** (`info tlb 0x100000` is rejected with
  `tlb: extraneous characters at the end of line`); newer upstream work adds
  `info tlb [start [end]]`.
* **`info registers`**: `CR0=... CR2=... CR3=... CR4=...` and `EFER=...` lines
  are stable; the segment lines (`CS =0008 ...`) and `RIP`/`RFL`/`HLT` layout
  are stable in 7.x.  `%`-free uppercase hex, no `0x`.
* **`info cpus`**: `* CPU #N: thread_id=NNNN`.  `thread_id` is a host PID and
  obviously changes every run — parse the shape, never compare the number.
* **Structured QMP** (`query-version`, `query-cpus-fast`,
  `query-status`, `query-memory-size-summary`) — prefer these over HMP text.
  `thread-id` / `cpu-index` are the stable fields; `qom-path` may change.

### Version-dependent / do not over-fit

* The **9 flag characters** of `info tlb` are version-dependent.  Empirically
  decoding this fixture's samples (and consistent with QEMU's `print_pte`
  flag order):

  | col | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 |
  |---|---|---|---|---|---|---|---|---|---|
  | meaning | NX | G | **PSE** | D | A | PCD | PWT | US | RW |

  The important correction: column 3 is the **PSE / page-size bit, not
  "present"**.  Proof from this fixture: every 4 KiB leaf PTE is present with
  RW set, yet prints `--------W`; only the 2 MiB huge-page leaves (PSE=1) print
  `--P-----W`.  A parser that treats column 3 as "present" will mis-classify
  every 4 KiB page.  Accessed/dirty pages print `----A---W` / `---DA---W`.
* `info mem` abbreviates the top of a range using the *end* address
  (`0000000000600000-0000000040000000`), i.e. end is exclusive.
* The FPU/XMM lines in `info registers` and the exact segment attribute hex are
  version-dependent; ignore them for assertions.
* `info tlb` output can be enormous on a real guest (here 1021 lines for a
  1 GiB identity map).  A parser must stream, not assume a small reply.

### What these samples demonstrate (non-degenerate)

`info mem` reproduces the fixture's map, including both deliberate holes:

```
0000000000000000-00000000001ff000 00000000001ff000 -rw   <- 4KiB pages, hole at 0x1ff000
0000000000200000-0000000000400000 0000000000200000 -rw   <- 2MiB huge page
0000000000600000-0000000040000000 000000003fa00000 -rw   <- huge pages (gap 0x400000..0x600000)
```

`info registers` proves paging is on at capture time:
`CR0=80000011` (PG=1), `CR3=0000000000104000`, `CR2=0000000000400000`.

## Limitations

* The samples are from **one** QEMU version (`7.2.22`, Debian).
  `query-version` is recorded alongside so a consumer can detect drift.
* The guest is **halted** when the samples are taken (it must be, to make the
  capture point deterministic).  A live/running guest can produce additional
  mappings (e.g. after the kernel touches more pages), so don't treat these
  line counts as universal.
* The `thread_id` in `info cpus` and `thread-id` in `qmp-query-cpus-fast.json`
  differ between runs and are regenerated each time; they are not golden values.
