#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

mod accounts;
#[path = "arch/mod.rs"]
mod arch;
#[path = "ipc.rs"]
mod ipc;
#[path = "drivers/ata.rs"]
pub mod ata;
#[path = "drivers/block.rs"]
pub mod block;
#[path = "../../bootstrap/boot_info.rs"]
mod boot_info;
#[path = "config.rs"]
mod config;
mod crypto;
#[path = "fs/dfs.rs"]
mod dfs;
#[path = "exec/elf.rs"]
mod elf;
#[path = "console/framebuffer.rs"]
mod framebuffer;
#[path = "arch/x86_64/fpu.rs"]
mod fpu;
#[path = "arch/x86_64/gdt.rs"]
mod gdt;
#[path = "fs/gpt.rs"]
pub mod gpt;
#[path = "mm/heap.rs"]
mod heap;
#[path = "arch/x86_64/idt.rs"]
mod idt;
#[path = "fs/installer.rs"]
mod installer;
#[path = "kernel_log.rs"]
mod kernel_log;
#[path = "drivers/keyboard.rs"]
mod keyboard;
#[path = "mm/memory.rs"]
mod memory;
#[path = "arch/x86_64/paging.rs"]
mod paging;
#[path = "fs/permissions.rs"]
mod permissions;
#[path = "power.rs"]
mod power;
#[path = "exec/process.rs"]
mod process;
#[path = "fs/ramfs.rs"]
mod ramfs;
#[path = "fs/vfs.rs"]
mod vfs;
#[path = "abi/syscall.rs"]
mod syscall;
#[path = "time.rs"]
mod time;
#[path = "console/utf8.rs"]
mod utf8;

static LOG_LINE_START: AtomicBool = AtomicBool::new(true);
static EMBEDDED_FONT: &[u8] = include_bytes!("../../tools/assets/zap-vga16.psf");

#[no_mangle]
pub extern "C" fn kernel_entry(boot_info: *const boot_info::BootInfo) -> ! {
    unsafe {
        asm!("cli", options(nomem, nostack));
    }
    fpu::init();

    serial_init();
    if boot_info.is_null() {
        serial_write(b"KERNEL PANIC: bootstrap supplied no boot information\r\n");
        halt();
    }
    let boot_info = unsafe { &*boot_info };
    time::init_with(boot_info.tsc_start, boot_info.tsc_frequency);
    mask_legacy_pic();
    gdt::init();
    idt::init();

    let frame_count = memory::init(
        unsafe { boot_info.memory_entries() },
        boot_info.hhdm_offset,
        unsafe { boot_info.reserved() },
    );
    let Some(boot_frame) = memory::allocate_frame() else {
        console_write("No usable physical frames\n");
        halt();
    };
    unsafe {
        core::ptr::write_bytes(boot_frame.virtual_address, 0, 4096);
    }
    framebuffer::init(
        (!boot_info.framebuffer.is_null())
            .then(|| unsafe { &*boot_info.framebuffer })
            .and_then(|response| response.framebuffers().next()),
    );
    if !framebuffer::load_font(EMBEDDED_FONT) {
        kernel_panic("embedded .psf is invalid!\n");
    }
    console_write("ImagineOS Astrid w/ Dreamcore Kernel\n");
    console_write("Frame allocator ready; usable frames: ");
    console_number(frame_count.saturating_sub(1) as u64);
    console_write(";\nreserved boot frame at physical 0x");
    console_hex(boot_frame.physical_address);
    console_write("\n");
    power::init(boot_info.rsdp_address, boot_info.hhdm_offset);

    let disk_ready = match ata::init() {
        Ok(sectors) => {
            console_write("ATA primary master ready; sectors: ");
            console_number(sectors);
            console_write("\n");
            match gpt::Gpt::read_primary(&ata::PrimaryMaster) {
                Ok(table) => {
                    match table.find_partition(&ata::PrimaryMaster, &gpt::DFS_PARTITION_TYPE_GUID) {
                        Ok(partition) => {
                            console_write("GPT DFS partition found at LBA ");
                            console_number(partition.first_lba);
                            console_write("\n");
                        }
                        Err(gpt::GptError::PartitionNotFound) => {
                            console_write("GPT detected; no DFS partition found\n");
                        }
                        Err(_) => console_write("GPT DFS partition lookup failed\n"),
                    }
                }
                Err(gpt::GptError::InvalidSignature) => {
                    console_write("ATA disk has no primary GPT\n");
                }
                Err(_) => console_write("ATA disk GPT is invalid or unreadable\n"),
            }
            true
        }
        Err(_) => {
            console_write("No usable ATA primary-master disk; continuing without storage\n");
            false
        }
    };

    let ramfs_image: Option<&'static [u8]> = (!boot_info.modules.is_null())
        .then(|| unsafe { &*boot_info.modules })
        .and_then(|response| {
            response
                .modules()
                .iter()
                .find(|module| module.path().to_bytes().ends_with(b"ramfs.tar"))
        })
        .map(|module| unsafe {
            core::slice::from_raw_parts(module.addr(), module.size() as usize)
        });

    let Some(ramfs_image) = ramfs_image else {
        console_write("RAMFS ramfs.tar missing; stopping safely\n");
        halt();
    };
    ramfs::mount(ramfs_image);
    ramfs::set_block_disk_present(disk_ready);
    match dfs::mount_primary() {
        Ok(Some(_)) => console_write("DFS persistent root mounted successfully\n"),
        Ok(None) => console_write("No DFS partition; continuing with RAMFS root\n"),
        Err(_) => console_write("DFS mount or journal recovery failed; using RAMFS root\n"),
    }
    if let Some(settings) = ramfs::read(config::GLOBAL_CONFIG_PATH) {
        if config::load(settings).is_err() {
            console_write("Global configuration invalid; using defaults\n");
        }
    } else {
        console_write("Global configuration missing; using defaults\n");
    }
    if let Some(font) = ramfs::find_font() {
        if !framebuffer::load_font(font) {
            console_write("RAMFS PSF invalid; retaining embedded zap-vga32.psf\n");
        }
    } else {
        console_write("RAMFS PSF not found; retaining embedded zap-vga32.psf\n");
    }
    let Some(init_program) = ramfs::read("/sbin/init") else {
        kernel_panic("required /sbin/init not found in the active root filesystem\n");
    };
    console_write("Loading /sbin/init as PID 1 in ring 3\n");
    if process::init(&[(init_program, 1)]).is_err() {
        console_write("ELF loader failed; stopping safely\n");
        halt();
    }
    time::configure_timer_interrupt();
    console_write("Timer configured; preemption activates on the first ring-3 entry\n");
    // Interrupts stay disabled here on purpose. `process::start` enters ring 3
    // through an iretq whose saved RFLAGS has IF=1, so the timer can only fire
    // once a valid user trap frame is on the stack.
    process::start()
}

pub(crate) fn console_write(text: &str) {
    let mut remaining = text.as_bytes();
    while !remaining.is_empty() {
        if LOG_LINE_START.swap(false, Ordering::Relaxed) {
            let mut timestamp = [0; 32];
            let timestamp = time::format_elapsed(&mut timestamp);
            kernel_log::append(timestamp);
            serial_write(timestamp);
            framebuffer::write_utf8(timestamp);
        }
        let length = remaining
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(remaining.len(), |newline| newline + 1);
        let line = &remaining[..length];
        kernel_log::append(line);
        serial_write(line);
        framebuffer::write_utf8(line);
        if line.last() == Some(&b'\n') {
            LOG_LINE_START.store(true, Ordering::Relaxed);
        }
        remaining = &remaining[length..];
    }
}

pub(crate) fn console_write_bytes(bytes: &[u8]) {
    serial_write(bytes);
    framebuffer::write_ansi(bytes);
}

pub(crate) fn serial_log_timestamp() {
    let mut timestamp = [0; 32];
    serial_write(time::format_elapsed(&mut timestamp));
}

fn console_number(mut value: u64) {
    let mut digits = [0u8; 20];
    let mut cursor = digits.len();
    if value == 0 {
        serial_write(b"0");
        framebuffer::write_char('0');
        return;
    }
    while value != 0 {
        cursor -= 1;
        digits[cursor] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    serial_write(&digits[cursor..]);
    if let Ok(text) = core::str::from_utf8(&digits[cursor..]) {
        framebuffer::write_str(text);
    }
}

#[cfg(feature = "kernel-debug")]
pub(crate) fn console_write_number(value: u64) {
    console_number(value);
}

fn console_hex(mut value: u64) {
    let mut digits = [0u8; 16];
    let mut cursor = digits.len();
    while value != 0 {
        cursor -= 1;
        let digit = (value & 0xf) as u8;
        digits[cursor] = if digit < 10 {
            b'0' + digit
        } else {
            b'a' + digit - 10
        };
        value >>= 4;
    }
    if cursor == digits.len() {
        cursor -= 1;
        digits[cursor] = b'0';
    }
    serial_write(&digits[cursor..]);
    if let Ok(text) = core::str::from_utf8(&digits[cursor..]) {
        framebuffer::write_str(text);
    }
}

fn serial_init() {
    unsafe {
        out(0x3f9, 0x00);
        out(0x3fb, 0x80);
        out(0x3f8, 0x03);
        out(0x3f9, 0x00);
        out(0x3fb, 0x03);
        out(0x3fa, 0xc7);
        out(0x3fc, 0x0b);
    }
}

fn mask_legacy_pic() {
    unsafe {
        out(0x21, 0xff);
        out(0xa1, 0xff);
    }
}

/// Escreve um valor hexadecimal de 16 dígitos na serial (diagnóstico).
#[cfg(feature = "kernel-debug")]
pub(crate) fn serial_hex(mut value: u64) {
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

pub(crate) fn serial_write(bytes: &[u8]) {
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
        out(0x3f8, byte);
    }
}

unsafe fn out(port: u16, value: u8) {
    asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
}

unsafe fn in_port(port: u16) -> u8 {
    let value: u8;
    asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    value
}

fn halt() -> ! {
    loop {
        unsafe {
            asm!("hlt", options(nomem, nostack));
        }
    }
}

pub(crate) fn kernel_halt() -> ! {
    halt()
}

fn kernel_panic(message: &str) -> ! {
    console_write("KERNEL PANIC: ");
    console_write(message);
    halt()
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    kernel_panic("unrecoverable kernel error\n")
}
