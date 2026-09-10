# PrincessIDE reference kernel — P0 test fixture.
#
#   make            -> build/refkernel.elf   (Multiboot image, ELF64, -g + symbols)
#   make iso        -> build/refkernel.iso   (GRUB Multiboot2 ISO)
#   make symbols    -> print the fault-probe symbol table entry
#   make clean
#
# Freestanding C + assembly only; no third party dependencies, no libc.

BUILD  := build
TARGET := $(BUILD)/refkernel.elf
ISO    := $(BUILD)/refkernel.iso

CC := gcc
LD := ld

# -O0 -g            : debuggable, and the only optimisation level used by the
#                     fixture so that the faulting instruction stays put.
# -mgeneral-regs-only: no SSE/MMX, so the kernel needs no FPU state handling.
# -mno-red-zone     : required with interrupt handlers on the interrupted stack.
CFLAGS  := -m64 -std=gnu11 -O0 -g \
           -ffreestanding -fno-builtin -fno-stack-protector \
           -fno-pic -fno-pie -fno-asynchronous-unwind-tables -fno-unwind-tables \
           -fcf-protection=none -mno-red-zone -mgeneral-regs-only \
           -Wall -Wextra

ASFLAGS := -m64 -g
LDFLAGS := -m elf_x86_64 -T linker.ld --nostdlib --build-id=none

OBJS := $(BUILD)/boot.o $(BUILD)/isr.o $(BUILD)/serial.o $(BUILD)/idt.o $(BUILD)/kernel.o

.PHONY: all iso symbols check clean

all: $(TARGET)

$(BUILD):
	mkdir -p $(BUILD)

$(BUILD)/%.o: %.c | $(BUILD)
	$(CC) $(CFLAGS) -c $< -o $@

$(BUILD)/%.o: %.S | $(BUILD)
	$(CC) $(ASFLAGS) -c $< -o $@

$(TARGET): $(OBJS) linker.ld
	$(LD) $(LDFLAGS) -o $@ $(OBJS)
	@echo "[refkernel] built $@"

iso: $(ISO)

$(ISO): $(TARGET) grub.cfg
	mkdir -p $(BUILD)/iso/boot/grub
	cp $(TARGET) $(BUILD)/iso/boot/refkernel.elf
	cp grub.cfg $(BUILD)/iso/boot/grub/grub.cfg
	grub-mkrescue --compress=xz -o $(ISO) $(BUILD)/iso
	@echo "[refkernel] built $@"

symbols: $(TARGET)
	@nm -n $(TARGET) | grep -E 'refkernel_fault_probe|kernel_main|exception_dispatch|_start'

check: $(TARGET)
	@file $(TARGET)
	@readelf -h $(TARGET) | grep -E 'Class|Machine|Entry'
	@nm $(TARGET) | grep -c . | sed 's/^/[refkernel] symbol count: /'

clean:
	rm -rf $(BUILD)
