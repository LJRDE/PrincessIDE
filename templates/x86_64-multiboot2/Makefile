# PrincessIDE kernel project template — x86_64 + Multiboot2 + GRUB ISO.
#
#   make        -> build/kernel.elf   (Multiboot2 image: ELF64, -g + symbols)
#   make iso    -> build/kernel.iso   (GRUB Multiboot2 ISO)
#   make run    -> build the ISO, boot it headless in QEMU, assert the banner
#   make clean
#
# The build is freestanding C + assembly only: no libc, no third-party code.
#
# Toolchain: this Makefile expects the PrincessIDE environment to be active,
# i.e. run it from a shell where `source scripts/env.sh` has been executed.
# That provides qemu-system-x86_64, xorriso/mtools (needed by grub-mkrescue) and
# $PRINCESSIDE_QEMU_DATA (QEMU's firmware/option-ROM data directory).  gcc, as,
# ld and grub-mkrescue come from the host.
#
# Concurrency: keep `make -j2` as the upper bound (the host has no swap).

BUILD  := build
TARGET := $(BUILD)/kernel.elf
ISO    := $(BUILD)/kernel.iso

CC := gcc
AS := as
LD := ld

# -O0 -g              : keep the image debuggable
# -ffreestanding ...  : no libc assumptions, no stack protector, no PIE
# -mno-red-zone       : required once interrupts are handled
# -mgeneral-regs-only : no SSE/MMX, so no FPU state handling is needed
CFLAGS  := -m64 -std=gnu11 -O0 -g \
           -ffreestanding -fno-builtin -fno-stack-protector \
           -fno-pic -fno-pie -fno-asynchronous-unwind-tables -fno-unwind-tables \
           -fcf-protection=none -mno-red-zone -mgeneral-regs-only \
           -Wall -Wextra

ASFLAGS := --64 -g
LDFLAGS := -m elf_x86_64 -T linker.ld --nostdlib --build-id=none

OBJS := $(BUILD)/boot.o $(BUILD)/serial.o $(BUILD)/kernel.o

# Headless QEMU invocation.  The fixed serial contract is
#   -display none -serial stdio -monitor none
# and, per decision D2, the boot path is the GRUB ISO (-cdrom + -boot d), never
# `-kernel`.  run.sh consumes exactly these flags.
BANNER      := PrincessIDE template kernel booted
BOOT_TIMEOUT ?= 30
MEMORY       ?= 256M
QEMU         := qemu-system-x86_64
QEMUFLAGS    := -m $(MEMORY) -cdrom $(ISO) -boot d \
                -display none -serial stdio -monitor none -no-reboot

.PHONY: all iso run clean help

all: $(TARGET)

$(BUILD):
	mkdir -p $(BUILD)

$(BUILD)/%.o: %.c | $(BUILD)
	$(CC) $(CFLAGS) -c $< -o $@

$(BUILD)/%.o: %.s | $(BUILD)
	$(AS) $(ASFLAGS) -o $@ $<

$(TARGET): $(OBJS) linker.ld
	$(LD) $(LDFLAGS) -o $@ $(OBJS)
	@echo "[template] built $@"

iso: $(ISO)

$(ISO): $(TARGET) grub.cfg
	mkdir -p $(BUILD)/iso/boot/grub
	cp $(TARGET) $(BUILD)/iso/boot/kernel.elf
	cp grub.cfg $(BUILD)/iso/boot/grub/grub.cfg
	grub-mkrescue --compress=xz -o $(ISO) $(BUILD)/iso
	@echo "[template] built $@"

# Boots the ISO with no display, captures COM1 to build/serial.log and exits 0
# only when the banner shows up.  Needs the env.sh shell (see header).
run: $(ISO)
	BOOT_TIMEOUT=$(BOOT_TIMEOUT) MEMORY=$(MEMORY) BANNER="$(BANNER)" bash run.sh

clean:
	rm -rf $(BUILD)

help:
	@echo "targets: all (default) | iso | run | clean"
	@echo "note: source scripts/env.sh before invoking make / make iso / make run"
