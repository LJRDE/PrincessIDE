# PrincessIDE paging kernel — P5/P4 test fixture.
#
#   make            -> build/pagingkernel.elf  (Multiboot2 image, ELF64, -g + symbols)
#   make iso        -> build/pagingkernel.iso  (GRUB Multiboot2 ISO)
#   make run        -> boot the ISO headless and assert the banner + #PF
#   make symbols    -> print the page-fault-probe symbol table entry
#   make check      -> file/readelf/nm sanity checks
#   make clean
#
# Freestanding C + assembly only; no third party dependencies, no libc.
#
# The build expects the PrincessIDE environment to be active (source
# scripts/env.sh) for `make iso` / `make run`: that is what provides
# qemu-system-x86_64, xorriso/mtools (used by grub-mkrescue) and
# $PRINCESSIDE_QEMU_DATA.  gcc/ld/grub-mkrescue come from the host.
#
# Concurrency: keep `make -j2` as the upper bound (host has no swap, D16).

BUILD  := build
TARGET := $(BUILD)/pagingkernel.elf
ISO    := $(BUILD)/pagingkernel.iso

CC := gcc
LD := ld

# -O0 -g              : debuggable, and the faulting instruction stays put.
# -mgeneral-regs-only : no SSE/MMX, so no FPU state handling is needed.
# -mno-red-zone       : required with interrupt handlers on the interrupted stack.
CFLAGS  := -m64 -std=gnu11 -O0 -g \
           -ffreestanding -fno-builtin -fno-stack-protector \
           -fno-pic -fno-pie -fno-asynchronous-unwind-tables -fno-unwind-tables \
           -fcf-protection=none -mno-red-zone -mgeneral-regs-only \
           -Wall -Wextra

ASFLAGS := -m64 -g
LDFLAGS := -m elf_x86_64 -T linker.ld --nostdlib --build-id=none

OBJS := $(BUILD)/boot.o $(BUILD)/isr.o $(BUILD)/serial.o $(BUILD)/idt.o \
        $(BUILD)/paging.o $(BUILD)/kernel.o

# Headless QEMU invocation (D9): -display none -serial stdio -monitor none.
# Boot path (D2): GRUB Multiboot2 ISO via -cdrom + -boot d, never `-kernel`.
BANNER      := PrincessIDE paging kernel booted
FAULT_MARK  := FAULT_RIP=
BOOT_TIMEOUT ?= 45
MEMORY       ?= 256M

.PHONY: all iso run symbols check clean help

all: $(TARGET)

$(BUILD):
	mkdir -p $(BUILD)

$(BUILD)/%.o: %.c | $(BUILD)
	$(CC) $(CFLAGS) -c $< -o $@

$(BUILD)/%.o: %.S | $(BUILD)
	$(CC) $(ASFLAGS) -c $< -o $@

$(TARGET): $(OBJS) linker.ld
	$(LD) $(LDFLAGS) -o $@ $(OBJS)
	@echo "[pagingkernel] built $@"

iso: $(ISO)

$(ISO): $(TARGET) grub.cfg
	mkdir -p $(BUILD)/iso/boot/grub
	cp $(TARGET) $(BUILD)/iso/boot/pagingkernel.elf
	cp grub.cfg $(BUILD)/iso/boot/grub/grub.cfg
	grub-mkrescue --compress=xz -o $(ISO) $(BUILD)/iso
	@echo "[pagingkernel] built $@"

run: $(ISO)
	BOOT_TIMEOUT=$(BOOT_TIMEOUT) MEMORY=$(MEMORY) \
	BANNER="$(BANNER)" FAULT_MARK="$(FAULT_MARK)" bash run.sh

symbols: $(TARGET)
	@nm -n $(TARGET) | grep -E 'paging_fault_probe|kernel_main|exception_dispatch|_start|boot_pml4'

check: $(TARGET)
	@file $(TARGET)
	@readelf -h $(TARGET) | grep -E 'Class|Machine|Entry'
	@nm $(TARGET) | grep -c . | sed 's/^/[pagingkernel] symbol count: /'

clean:
	rm -rf $(BUILD)

help:
	@echo "targets: all (default) | iso | run | symbols | check | clean"
	@echo "note: source scripts/env.sh before invoking make iso / make run"
