#![allow(dead_code)]
use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "bootstrap")]
const PIT_RELOAD: u64 = u16::MAX as u64;
#[cfg(feature = "bootstrap")]
const POLL_LIMIT: usize = 100_000_000;
// Frequency of the PIT input clock, also used to program the periodic
// scheduling tick in the kernel (see `configure_timer_interrupt`).
const PIT_INPUT_FREQUENCY: u64 = 1_193_182;
/// Target frequency of the scheduler tick installed by the kernel.
pub const TIMER_TICK_HZ: u64 = 100;
/// Quantos ticks cada processo recebe antes de o timer pedir a troca.
///
/// Sem isto o processo era preemptado a cada 10 ms, e o custo do switch
/// (trocar CR3, salvar/restaurar o estado FPU) consumia o pouco tempo útil —
/// um processo recém-criado por `fork` era interrompido antes de executar a
/// primeira instrução de usuário e parecia travar. 5 ticks dão 50 ms de
/// quantum, que é generoso para o modelo cooperativo que este kernel usa.
pub const TIMER_QUANTUM_TICKS: u64 = 10;

static START_TSC: AtomicU64 = AtomicU64::new(0);
static TSC_FREQUENCY: AtomicU64 = AtomicU64::new(0);
static TIMER_TICKS: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "bootstrap")]
pub fn init() -> (u64, u64) {
    let start = read_tsc();
    let frequency = calibrate_tsc().or_else(cpuid_tsc_frequency).unwrap_or(0);
    init_with(start, frequency);
    (start, frequency)
}

pub fn init_with(start: u64, frequency: u64) {
    START_TSC.store(start, Ordering::Relaxed);
    TSC_FREQUENCY.store(frequency, Ordering::Relaxed);
}

/// Programs the 8253/8254 PIT channel 0 to raise IRQ0 at [`TIMER_TICK_HZ`] and
/// unmasks that line on the legacy PIC. Must run after the GDT/IDT are ready and
/// before interrupts are enabled with `sti`.
pub fn configure_timer_interrupt() {
    let reload = PIT_INPUT_FREQUENCY / TIMER_TICK_HZ;
    let reload = reload.clamp(1, u16::MAX as u64) as u16;
    unsafe {
        remap_pic();
        // Command: channel 0, lobyte/hibyte, mode 2 (rate generator), binary.
        out_byte(0x43, 0x34);
        out_byte(0x40, reload as u8);
        out_byte(0x40, (reload >> 8) as u8);
        // Unmask IRQ0 (bit 0) on the master PIC, keeping the other lines masked.
        let mask = in_byte(0x21) & !0x01;
        out_byte(0x21, mask);
        // Mask every line on the slave PIC; only IRQ0 is wanted for now.
        out_byte(0xa1, 0xff);
    }
}

/// Reprograms the legacy 8259 PICs so hardware IRQ0-15 map to vectors 0x20-0x2f.
///
/// The firmware leaves the PICs in 8086 mode, where IRQ0 maps to vector 0x08 and
/// collides with the CPU exception vectors. Without this remap an IRQ0 would be
/// delivered as vector 0x08 (double fault) instead of the IDT entry at 0x20.
unsafe fn remap_pic() {
    const PIC1: u16 = 0x20;
    const PIC1_COMMAND: u16 = 0x20;
    const PIC1_DATA: u16 = 0x21;
    const PIC2_COMMAND: u16 = 0xa0;
    const PIC2_DATA: u16 = 0xa1;
    const ICW1_INIT: u8 = 0x11;
    const ICW4_8086: u8 = 0x01;

    let saved1 = in_byte(PIC1_DATA);
    let saved2 = in_byte(PIC2_DATA);
    out_byte(PIC1_COMMAND, ICW1_INIT); // start initialization sequence
    out_byte(PIC2_COMMAND, ICW1_INIT);
    out_byte(PIC1_DATA, 0x20); // master vector offset = 0x20 (IRQ0-7)
    out_byte(PIC2_DATA, 0x28); // slave vector offset = 0x28 (IRQ8-15)
    out_byte(PIC1_DATA, 0x04); // slave on IRQ2
    out_byte(PIC2_DATA, 0x02); // cascade identity
    out_byte(PIC1_DATA, ICW4_8086);
    out_byte(PIC2_DATA, ICW4_8086);
    // Restore the previous masks; the caller unmasks only IRQ0.
    out_byte(PIC1_DATA, saved1 | 0x01);
    out_byte(PIC2_DATA, saved2);
}

/// Reads a byte from an x86 I/O port.
unsafe fn in_byte(port: u16) -> u8 {
    let value: u8;
    asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    value
}

/// Records one scheduler tick. Invoked from the timer interrupt handler.
#[inline]
pub fn note_timer_tick() {
    TIMER_TICKS.fetch_add(1, Ordering::Relaxed);
}

/// Number of timer ticks observed since boot.
#[inline]
pub fn timer_ticks() -> u64 {
    TIMER_TICKS.load(Ordering::Relaxed)
}

pub fn elapsed_microseconds() -> Option<u64> {
    let frequency = TSC_FREQUENCY.load(Ordering::Relaxed);
    if frequency == 0 {
        return None;
    }
    let cycles = read_tsc().saturating_sub(START_TSC.load(Ordering::Relaxed));
    Some(
        cycles / frequency * 1_000_000
            + cycles % frequency * 1_000_000 / frequency,
    )
}

#[cfg(any(feature = "bootstrap", not(feature = "bootstrap")))]
pub fn format_elapsed(output: &mut [u8; 32]) -> &[u8] {
    let frequency = TSC_FREQUENCY.load(Ordering::Relaxed);
    if frequency == 0 {
        return b"[time?] ";
    }

    let cycles = read_tsc().saturating_sub(START_TSC.load(Ordering::Relaxed));
    let milliseconds = cycles / frequency * 1000 + (cycles % frequency) * 1000 / frequency;
    format_milliseconds(milliseconds, output)
}

#[cfg(any(feature = "bootstrap", not(feature = "bootstrap")))]
fn format_milliseconds(milliseconds: u64, output: &mut [u8; 32]) -> &[u8] {
    let seconds = milliseconds / 1000;
    let mut cursor = 0;
    output[cursor] = b'[';
    cursor += 1;
    cursor += write_decimal(&mut output[cursor..], seconds, 6);
    output[cursor] = b'.';
    cursor += 1;
    cursor += write_decimal(&mut output[cursor..], milliseconds % 1000, 3);
    output[cursor] = b']';
    output[cursor + 1] = b' ';
    &output[..cursor + 2]
}

#[cfg(feature = "bootstrap")]
fn calibrate_tsc() -> Option<u64> {
    let previous_control = unsafe { in_byte(0x61) };
    unsafe {
        out_byte(0x61, previous_control & !0x03);
        out_byte(0x43, 0xb0);
        out_byte(0x42, PIT_RELOAD as u8);
        out_byte(0x42, (PIT_RELOAD >> 8) as u8);
        out_byte(0x61, (previous_control & !0x02) | 0x01);
    }

    let mut remaining = POLL_LIMIT;
    while unsafe { in_byte(0x61) } & 0x20 != 0 && remaining != 0 {
        remaining -= 1;
        core::hint::spin_loop();
    }

    let start = read_tsc();
    while unsafe { in_byte(0x61) } & 0x20 == 0 && remaining != 0 {
        remaining -= 1;
        core::hint::spin_loop();
    }
    let end = read_tsc();
    unsafe {
        out_byte(0x61, previous_control);
    }

    if remaining == 0 || end <= start {
        return None;
    }
    Some((end - start) * PIT_INPUT_FREQUENCY / PIT_RELOAD)
}

#[cfg(feature = "bootstrap")]
fn cpuid_tsc_frequency() -> Option<u64> {
    let maximum_leaf = core::arch::x86_64::__cpuid(0).eax;
    if maximum_leaf >= 0x15 {
        let leaf = core::arch::x86_64::__cpuid_count(0x15, 0);
        if leaf.eax != 0 && leaf.ebx != 0 && leaf.ecx != 0 {
            return Some((leaf.ecx as u64) * (leaf.ebx as u64) / (leaf.eax as u64));
        }
    }
    if maximum_leaf >= 0x16 {
        let nominal_mhz = core::arch::x86_64::__cpuid(0x16).eax;
        if nominal_mhz != 0 {
            return Some(nominal_mhz as u64 * 1_000_000);
        }
    }
    None
}

fn read_tsc() -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!(
            "rdtsc",
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags)
        );
    }
    ((high as u64) << 32) | low as u64
}

#[cfg(any(feature = "bootstrap", not(feature = "bootstrap")))]
fn write_decimal(output: &mut [u8], mut value: u64, minimum_digits: usize) -> usize {
    let mut digits = [0u8; 20];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    while digits.len() - start < minimum_digits {
        start -= 1;
        digits[start] = b'0';
    }
    let length = digits.len() - start;
    output[..length].copy_from_slice(&digits[start..]);
    length
}

unsafe fn out_byte(port: u16, value: u8) {
    asm!(
        "out dx, al",
        in("dx") port,
        in("al") value,
        options(nomem, nostack, preserves_flags)
    );
}
