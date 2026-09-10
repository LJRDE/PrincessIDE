# x86_64 + Multiboot2 kernel template

A minimal, working x86_64 operating-system kernel project for PrincessIDE.  It
boots through **GRUB via a Multiboot2 ISO** and prints a banner on **COM1**, then
halts.  It is deliberately small: no paging management, no interrupts, no memory
allocator — just the smallest thing that is genuinely bootable and debuggable.

```
PrincessIDE template kernel booted
```

That banner is intentionally **different** from the reference-kernel fixture
(`fixtures/refkernel/`, banner `PrincessIDE reference kernel booted`) so the two
can never be confused in tests.  The template does not use any file from the
fixture.

## Files

| File | Purpose |
| --- | --- |
| `boot.s` | Multiboot2 header, 32-bit entry, GDT, identity page tables, long-mode transition |
| `kernel.c` | `kernel_main()`: the banner and a couple of diagnostic lines |
| `serial.c` / `serial.h` | Polled 16550 COM1 driver (no libc) |
| `linker.ld` | ELF64 layout at 1 MiB (`R E` + `RW` segments, 64 KiB stack) |
| `grub.cfg` | GRUB menu entry using `multiboot2` |
| `Makefile` | `make` / `make iso` / `make run` / `make clean` |
| `run.sh` | Headless QEMU runner used by `make run` |
| `princess.toml` | PrincessIDE project descriptor (see `docs/spec/10-contracts.md` §4) |

## Dependencies

Everything comes from the PrincessIDE environment (`scripts/env.sh`):

- `gcc`, `as`, `ld` — build the freestanding kernel (host tools)
- `grub-mkrescue` (with `xorriso` + `mtools`) — produce the bootable ISO
- `qemu-system-x86_64` — run it headless

No display and no KVM are required; QEMU runs in TCG software emulation.

## Create a new project from this template

```bash
cp -a templates/x86_64-multiboot2 ~/mykernel
cd ~/mykernel
# edit princess.toml ([project].name), kernel.c, ...
```

## Build, make an ISO, run it headless

`make` needs the environment active (it provides QEMU and the GRUB ISO tools).
Each new shell must `source` it again:

```bash
source scripts/env.sh            # from the PrincessIDE repo root
cd templates/x86_64-multiboot2   # or your copied project

make -j2        # compile + link   -> build/kernel.elf
make -j2 iso    # GRUB Multiboot2  -> build/kernel.iso
make run        # boot headless, assert the banner, write build/serial.log
```

`make run` exits `0` only when the banner appears on the serial log; it prints
the captured output on failure and stops QEMU as soon as the banner is seen.
Use `make -j2` (never wider): the CI/development host runs without swap, so
wider fan-out can be killed by the OOM killer.

The raw QEMU command (what `run.sh` executes) is:

```bash
qemu-system-x86_64 -m 256M \
    -cdrom build/kernel.iso -boot d \
    -display none -serial stdio -monitor none -no-reboot
```

## Design rules this template follows

- **D2** — boot path is x86_64 + Multiboot2 + GRUB ISO (`-cdrom -boot d`), never
  `qemu -kernel` (QEMU's multiboot option ROM only accepts ELF32 images).
- **D9** — headless serial capture is always exactly
  `-display none -serial stdio -monitor none`; `-nographic` is not used.

## princess.toml

`princess.toml` is the declarative project descriptor read by the PrincessIDE
engine.  Field names are frozen by `docs/spec/10-contracts.md` §4; unknown keys
are a hard error.  Notable values:

- `[build].backend = "make"`, `[build].targets = ["all"]`
- `[run].boot = "multiboot2"`, `[run].args` carries the D9 serial flags and the
  GRUB ISO boot path
- `[run].serial.tee_to_file = "build/serial.log"` — where the engine tees COM1
- `[toolchain]` names the tools this Makefile actually calls (`gcc`/`as`/`ld`/`gdb`)

`compile_commands.json` is listed under `[build]` because it is part of the §4
schema; the IDE's build backend generates it for the C language service.  This
template does not commit one.

## Customising

- Change the banner in `kernel.c` if you copy this template (keep it unique).
- `kernel_main(uint32_t magic, uint32_t info)` receives the Multiboot2 boot magic
  and the physical address of the Multiboot2 information structure; parse the
  latter when you need the memory map (`boot.s` already requests it).
- The identity map covers `0x0..0x40000000` (1 GiB) with 2 MiB pages — enough to
  reach the kernel's 1 MiB load address and low MMIO.

## Known limitations

- Single CPU, no interrupt handling, no APIC, no paging beyond the boot identity
  map, no allocator, no userspace.  These are intentional — extend as needed.
- `-O0 -g` only: the image is debuggable, not optimised.
- `make` / `make iso` / `make run` must be run from a shell where
  `scripts/env.sh` has been sourced (the Makefile header says so too).
