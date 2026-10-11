use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::mem::size_of;

global_asm!(
    r#"
.global dreamcore_exception_0
dreamcore_exception_0: push 0; push 0; jmp dreamcore_exception_common
.global dreamcore_exception_1
dreamcore_exception_1: push 0; push 1; jmp dreamcore_exception_common
.global dreamcore_exception_2
dreamcore_exception_2: push 0; push 2; jmp dreamcore_exception_common
.global dreamcore_exception_3
dreamcore_exception_3: push 0; push 3; jmp dreamcore_exception_common
.global dreamcore_exception_4
dreamcore_exception_4: push 0; push 4; jmp dreamcore_exception_common
.global dreamcore_exception_5
dreamcore_exception_5: push 0; push 5; jmp dreamcore_exception_common
.global dreamcore_exception_6
dreamcore_exception_6: push 0; push 6; jmp dreamcore_exception_common
.global dreamcore_exception_7
dreamcore_exception_7: push 0; push 7; jmp dreamcore_exception_common
.global dreamcore_exception_8
dreamcore_exception_8: push 8; jmp dreamcore_exception_common
.global dreamcore_exception_9
dreamcore_exception_9: push 0; push 9; jmp dreamcore_exception_common
.global dreamcore_exception_10
dreamcore_exception_10: push 10; jmp dreamcore_exception_common
.global dreamcore_exception_11
dreamcore_exception_11: push 11; jmp dreamcore_exception_common
.global dreamcore_exception_12
dreamcore_exception_12: push 12; jmp dreamcore_exception_common
.global dreamcore_exception_13
dreamcore_exception_13: push 13; jmp dreamcore_exception_common
.global dreamcore_exception_14
dreamcore_exception_14: push 14; jmp dreamcore_exception_common
.global dreamcore_exception_15
dreamcore_exception_15: push 0; push 15; jmp dreamcore_exception_common
.global dreamcore_exception_16
dreamcore_exception_16: push 0; push 16; jmp dreamcore_exception_common
.global dreamcore_exception_17
dreamcore_exception_17: push 17; jmp dreamcore_exception_common
.global dreamcore_exception_18
dreamcore_exception_18: push 0; push 18; jmp dreamcore_exception_common
.global dreamcore_exception_19
dreamcore_exception_19: push 0; push 19; jmp dreamcore_exception_common
.global dreamcore_exception_20
dreamcore_exception_20: push 0; push 20; jmp dreamcore_exception_common
.global dreamcore_exception_21
dreamcore_exception_21: push 21; jmp dreamcore_exception_common
.global dreamcore_exception_22
dreamcore_exception_22: push 0; push 22; jmp dreamcore_exception_common
.global dreamcore_exception_23
dreamcore_exception_23: push 0; push 23; jmp dreamcore_exception_common
.global dreamcore_exception_24
dreamcore_exception_24: push 0; push 24; jmp dreamcore_exception_common
.global dreamcore_exception_25
dreamcore_exception_25: push 0; push 25; jmp dreamcore_exception_common
.global dreamcore_exception_26
dreamcore_exception_26: push 0; push 26; jmp dreamcore_exception_common
.global dreamcore_exception_27
dreamcore_exception_27: push 0; push 27; jmp dreamcore_exception_common
.global dreamcore_exception_28
dreamcore_exception_28: push 0; push 28; jmp dreamcore_exception_common
.global dreamcore_exception_29
dreamcore_exception_29: push 29; jmp dreamcore_exception_common
.global dreamcore_exception_30
dreamcore_exception_30: push 30; jmp dreamcore_exception_common
.global dreamcore_exception_31
dreamcore_exception_31: push 0; push 31; jmp dreamcore_exception_common

dreamcore_exception_common:
    cli
    mov rdi, [rsp]
    mov rsi, [rsp + 8]
    mov rdx, [rsp + 16]
    mov rcx, [rsp + 24]
    mov r8, cr2
    and rsp, -16
    call dreamcore_exception_handler
1:  hlt
    jmp 1b

.global dreamcore_syscall_stub
dreamcore_syscall_stub:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    mov rdi, rsp
    and rsp, -16
    call dreamcore_syscall_dispatch
    mov rsp, rax
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    iretq

.global dreamcore_timer_interrupt
dreamcore_timer_interrupt:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    mov rdi, rsp
    and rsp, -16
    call dreamcore_timer_dispatch
    mov rsp, rax
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    iretq

.global dreamcore_resume_trap_frame
dreamcore_resume_trap_frame:
    mov rsp, rdi
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    iretq
"#
);

unsafe extern "C" {
    fn dreamcore_exception_0();
    fn dreamcore_exception_1();
    fn dreamcore_exception_2();
    fn dreamcore_exception_3();
    fn dreamcore_exception_4();
    fn dreamcore_exception_5();
    fn dreamcore_exception_6();
    fn dreamcore_exception_7();
    fn dreamcore_exception_8();
    fn dreamcore_exception_9();
    fn dreamcore_exception_10();
    fn dreamcore_exception_11();
    fn dreamcore_exception_12();
    fn dreamcore_exception_13();
    fn dreamcore_exception_14();
    fn dreamcore_exception_15();
    fn dreamcore_exception_16();
    fn dreamcore_exception_17();
    fn dreamcore_exception_18();
    fn dreamcore_exception_19();
    fn dreamcore_exception_20();
    fn dreamcore_exception_21();
    fn dreamcore_exception_22();
    fn dreamcore_exception_23();
    fn dreamcore_exception_24();
    fn dreamcore_exception_25();
    fn dreamcore_exception_26();
    fn dreamcore_exception_27();
    fn dreamcore_exception_28();
    fn dreamcore_exception_29();
    fn dreamcore_exception_30();
    fn dreamcore_exception_31();
    fn dreamcore_syscall_stub();
    fn dreamcore_timer_interrupt();
    fn dreamcore_resume_trap_frame(frame: *mut crate::syscall::TrapFrame) -> !;
}

/// Entry point for the periodic PIT interrupt (IRQ0). It preserves the interrupted
/// trap frame, hands control to the scheduler for possible preemption, then returns
/// the frame to resume on top of the iretq instruction.
#[unsafe(no_mangle)]
extern "C" fn dreamcore_timer_dispatch(frame: *mut crate::syscall::TrapFrame) -> *mut crate::syscall::TrapFrame {
    crate::arch::x86_64::timer::handle_interrupt(frame)
}


#[unsafe(no_mangle)]
extern "C" fn dreamcore_exception_handler(
    vector: u64,
    error: u64,
    rip: u64,
    cs: u64,
    cr2: u64,
) -> ! {
    if cs & 3 == 3 {
        crate::serial_log_timestamp();
        serial_write(b"User process fault; terminating PID ");
        serial_hex(crate::process::current_pid() as u64);
        serial_write(b" vector=0x");
        serial_hex(vector);
        serial_write(b" error=0x");
        serial_hex(error);
        serial_write(b" rip=0x");
        serial_hex(rip);
        serial_write(b" cr2=0x");
        serial_hex(cr2);
        serial_write(b"\r\n");
        let next = crate::process::terminate_faulting_process();
        unsafe {
            dreamcore_resume_trap_frame(next);
        }
    }
    crate::serial_log_timestamp();
    serial_write(b"FATAL EXCEPTION vector=0x");
    serial_hex(vector);
    serial_write(b" error=0x");
    serial_hex(error);
    serial_write(b" rip=0x");
    serial_hex(rip);
    serial_write(b" cs=0x");
    serial_hex(cs);
    serial_write(b" cr2=0x");
    serial_hex(cr2);
    serial_write(b"\r\n");
    loop {
        unsafe {
            asm!("hlt", options(nomem, nostack));
        }
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    attributes: u8,
    offset_middle: u16,
    offset_high: u32,
    reserved: u32,
}

impl IdtEntry {
    const MISSING: Self = Self {
        offset_low: 0,
        selector: 0,
        ist: 0,
        attributes: 0,
        offset_middle: 0,
        offset_high: 0,
        reserved: 0,
    };

    fn interrupt_gate(handler: u64, selector: u16) -> Self {
        Self {
            offset_low: handler as u16,
            selector,
            ist: 0,
            attributes: 0x8e,
            offset_middle: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }
}

struct SharedIdt(UnsafeCell<[IdtEntry; 256]>);
unsafe impl Sync for SharedIdt {}
static IDT: SharedIdt = SharedIdt(UnsafeCell::new([IdtEntry::MISSING; 256]));

#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

pub fn init() {
    let idt = unsafe { &mut *IDT.0.get() };
    let selector: u16;
    unsafe {
        asm!("mov {0:x}, cs", out(reg) selector, options(nomem, nostack, preserves_flags));
    }
    let handlers = [
        dreamcore_exception_0,
        dreamcore_exception_1,
        dreamcore_exception_2,
        dreamcore_exception_3,
        dreamcore_exception_4,
        dreamcore_exception_5,
        dreamcore_exception_6,
        dreamcore_exception_7,
        dreamcore_exception_8,
        dreamcore_exception_9,
        dreamcore_exception_10,
        dreamcore_exception_11,
        dreamcore_exception_12,
        dreamcore_exception_13,
        dreamcore_exception_14,
        dreamcore_exception_15,
        dreamcore_exception_16,
        dreamcore_exception_17,
        dreamcore_exception_18,
        dreamcore_exception_19,
        dreamcore_exception_20,
        dreamcore_exception_21,
        dreamcore_exception_22,
        dreamcore_exception_23,
        dreamcore_exception_24,
        dreamcore_exception_25,
        dreamcore_exception_26,
        dreamcore_exception_27,
        dreamcore_exception_28,
        dreamcore_exception_29,
        dreamcore_exception_30,
        dreamcore_exception_31,
    ];
    for (entry, handler) in idt.iter_mut().zip(handlers) {
        let handler = handler as *const () as usize as u64;
        *entry = IdtEntry::interrupt_gate(handler, selector);
    }
    let timer_handler = dreamcore_timer_interrupt as *const () as usize as u64;
    idt[0x20] = IdtEntry::interrupt_gate(timer_handler, selector);
    let syscall_handler = dreamcore_syscall_stub as *const () as usize as u64;
    idt[0x80] = IdtEntry {
        attributes: 0xee,
        ..IdtEntry::interrupt_gate(syscall_handler, selector)
    };
    let pointer = DescriptorTablePointer {
        limit: (size_of::<[IdtEntry; 256]>() - 1) as u16,
        base: idt.as_ptr() as u64,
    };
    unsafe {
        asm!("lidt [{}]", in(reg) &pointer, options(readonly, nostack, preserves_flags));
    }
}

fn serial_write(bytes: &[u8]) {
    let mut previous_was_cr = false;
    for &byte in bytes {
        if byte == b'\n' && !previous_was_cr {
            serial_write_byte(b'\r');
        }
        serial_write_byte(byte);
        previous_was_cr = byte == b'\r';
    }
}

fn serial_write_byte(byte: u8) {
    unsafe {
        while in_port(0x3fd) & 0x20 == 0 {
            asm!("pause", options(nomem, nostack, preserves_flags));
        }
        asm!("out dx, al", in("dx") 0x3f8u16, in("al") byte, options(nomem, nostack, preserves_flags));
    }
}

fn serial_hex(mut value: u64) {
    let mut digits = [0u8; 16];
    for index in (0..digits.len()).rev() {
        let digit = (value & 0xf) as u8;
        digits[index] = if digit < 10 {
            b'0' + digit
        } else {
            b'a' + digit - 10
        };
        value >>= 4;
    }
    serial_write(&digits);
}

unsafe fn in_port(port: u16) -> u8 {
    let value: u8;
    asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    value
}
