/* PrincessIDE paging kernel — CPU exception entry stubs.
 *
 * For every architectural exception vector 0..31 this emits a stub that
 * normalises the stack into the layout described by `struct exception_frame`
 * (see idt.h) and tail-calls the common C dispatcher.  Vector 14 (#PF) and
 * vector 6 (#UD) both have a dedicated stub; the #PF handler is the one that
 * reports the faulting data address (CR2).
 *
 * Stack layout produced for vector N, from the interrupted instruction outwards:
 *
 *      +152  rflags      } pushed by the CPU
 *      +144  cs          }
 *      +136  rip         }
 *      +128  error       <- real error code, or 0 pushed by the stub
 *      +120  vector      <- pushed by the stub
 *      +112  r15 ... +0 rax   <- pushed by isr_common
 *
 * The first field of `struct exception_frame` is therefore rax, i.e. the last
 * register pushed by isr_common.
 */

.section .text, "ax"
.code64

.macro ISR_ENTRY n, haserr
.global isr_stub_\n
.type isr_stub_\n, @function
isr_stub_\n:
.if \haserr == 0
    pushq   $0                      /* synthetic error code */
.endif
    pushq   $\n                     /* vector number        */
    jmp     isr_common
.size isr_stub_\n, . - isr_stub_\n
.endm

/*                      vector, has-error-code */
ISR_ENTRY   0, 0        /* #DE divide error                  */
ISR_ENTRY   1, 0        /* #DB debug                         */
ISR_ENTRY   2, 0        /*    NMI                             */
ISR_ENTRY   3, 0        /* #BP breakpoint                    */
ISR_ENTRY   4, 0        /* #OF overflow                      */
ISR_ENTRY   5, 0        /* #BR bound range                   */
ISR_ENTRY   6, 0        /* #UD invalid opcode                */
ISR_ENTRY   7, 0        /* #NM device not available          */
ISR_ENTRY   8, 1        /* #DF double fault                  */
ISR_ENTRY   9, 0        /*    coprocessor segment overrun    */
ISR_ENTRY  10, 1        /* #TS invalid TSS                   */
ISR_ENTRY  11, 1        /* #NP segment not present           */
ISR_ENTRY  12, 1        /* #SS stack-segment fault           */
ISR_ENTRY  13, 1        /* #GP general protection            */
ISR_ENTRY  14, 1        /* #PF page fault  <-- the one we take */
ISR_ENTRY  15, 0        /*    reserved                       */
ISR_ENTRY  16, 0        /* #MF x87 floating-point            */
ISR_ENTRY  17, 1        /* #AC alignment check               */
ISR_ENTRY  18, 0        /* #MC machine check                 */
ISR_ENTRY  19, 0        /* #XM SIMD floating-point           */
ISR_ENTRY  20, 0        /* #VE virtualisation exception      */
ISR_ENTRY  21, 1        /* #CP control protection            */
ISR_ENTRY  22, 0
ISR_ENTRY  23, 0
ISR_ENTRY  24, 0
ISR_ENTRY  25, 0
ISR_ENTRY  26, 0
ISR_ENTRY  27, 0
ISR_ENTRY  28, 0
ISR_ENTRY  29, 1        /* #VC VMM communication             */
ISR_ENTRY  30, 1        /* #SX security exception            */
ISR_ENTRY  31, 0

.type isr_common, @function
isr_common:
    /* Save the general purpose registers in reverse struct order so that the
     * last push (rax) sits at the lowest address. */
    pushq   %r15
    pushq   %r14
    pushq   %r13
    pushq   %r12
    pushq   %r11
    pushq   %r10
    pushq   %r9
    pushq   %r8
    pushq   %rbp
    pushq   %rdi
    pushq   %rsi
    pushq   %rdx
    pushq   %rcx
    pushq   %rbx
    pushq   %rax

    movq    %rsp, %rdi              /* arg 1: struct exception_frame * */
    call    exception_dispatch      /* never returns                   */

.Lisr_halt:
    cli
    hlt
    jmp     .Lisr_halt
.size isr_common, . - isr_common

.section .rodata
.align 8
.global isr_stub_table
isr_stub_table:
    .quad isr_stub_0
    .quad isr_stub_1
    .quad isr_stub_2
    .quad isr_stub_3
    .quad isr_stub_4
    .quad isr_stub_5
    .quad isr_stub_6
    .quad isr_stub_7
    .quad isr_stub_8
    .quad isr_stub_9
    .quad isr_stub_10
    .quad isr_stub_11
    .quad isr_stub_12
    .quad isr_stub_13
    .quad isr_stub_14
    .quad isr_stub_15
    .quad isr_stub_16
    .quad isr_stub_17
    .quad isr_stub_18
    .quad isr_stub_19
    .quad isr_stub_20
    .quad isr_stub_21
    .quad isr_stub_22
    .quad isr_stub_23
    .quad isr_stub_24
    .quad isr_stub_25
    .quad isr_stub_26
    .quad isr_stub_27
    .quad isr_stub_28
    .quad isr_stub_29
    .quad isr_stub_30
    .quad isr_stub_31

/* Mark the stack as non-executable for the linker. */
.section .note.GNU-stack, "", @progbits
