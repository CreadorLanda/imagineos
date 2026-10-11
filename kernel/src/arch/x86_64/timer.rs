//! Periodic timer interrupt (IRQ0) driven preemption.
//!
//! The PIT raises IRQ0 at [`crate::time::TIMER_TICK_HZ`]. The assembly stub in
//! `idt.rs` saves the interrupted trap frame and calls [`handle_interrupt`],
//! which asks the scheduler for a possible context switch and returns the frame
//! to resume on top of the `iretq` instruction.

use crate::syscall::TrapFrame;

/// Handles one timer tick.
///
/// This runs with interrupts disabled (the IDT gate is an interrupt gate). It
/// forwards the interrupted frame to the scheduler; if another process is
/// eligible, the scheduler switches address spaces and returns a different
/// frame, otherwise it returns `frame` unchanged so the interrupted process
/// continues transparently.
#[unsafe(no_mangle)]
pub extern "C" fn handle_interrupt(frame: *mut TrapFrame) -> *mut TrapFrame {
    crate::time::note_timer_tick();
    // The end-of-interrupt is sent to the master PIC so further IRQ0 (and any
    // other unmasked line) can be delivered while we are still in the handler.
    unsafe {
        out_byte(0x20, 0x20);
    }
    crate::process::timer_tick(frame)
}

/// Writes a byte to an x86 I/O port.
///
/// # Safety
/// The caller must ensure `port` is a valid I/O port for the operation.
pub unsafe fn out_byte(port: u16, value: u8) {
    core::arch::asm!(
        "out dx, al",
        in("dx") port,
        in("al") value,
        options(nomem, nostack, preserves_flags)
    );
}
