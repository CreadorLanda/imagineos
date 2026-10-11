#![no_std]
#![no_main]

use core::arch::asm;
use core::mem::MaybeUninit;
use core::panic::PanicInfo;
use core::ptr;
use limine::memory_map::{Entry, EntryType};
use limine::request::{
    FramebufferRequest, HhdmRequest, MemoryMapRequest, ModuleRequest, RsdpRequest,
};
use limine::response::{FramebufferResponse, MemoryMapResponse, ModuleResponse};
use limine::BaseRevision;

#[path = "boot_info.rs"]
mod boot_info;
#[path = "dzimage.rs"]
mod dzimage;
#[path = "../kernel/src/console/framebuffer.rs"]
mod framebuffer;
#[path = "../kernel/src/time.rs"]
mod time;

use boot_info::{BootInfo, PhysicalRange, MAX_RESERVED_RANGES};

const PAGE_SIZE: u64 = 4096;
const PAGE_MASK: u64 = !(PAGE_SIZE - 1);
const ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;
const PRESENT: u64 = 1;
const WRITABLE: u64 = 1 << 1;
const HUGE_PAGE: u64 = 1 << 7;
const MAX_LOAD_SEGMENTS: usize = 12;
const PAGE_TABLE_POOL_PAGES: usize = 32;
const MAX_PROGRAM_HEADERS: usize = 64;

#[used]
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();
#[used]
#[unsafe(link_section = ".requests")]
static MEMORY_MAP_REQUEST: MemoryMapRequest = MemoryMapRequest::new();
#[used]
#[unsafe(link_section = ".requests")]
static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();
#[used]
#[unsafe(link_section = ".requests")]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();
#[used]
#[unsafe(link_section = ".requests")]
static MODULE_REQUEST: ModuleRequest = ModuleRequest::new();
#[used]
#[unsafe(link_section = ".requests")]
static RSDP_REQUEST: RsdpRequest = RsdpRequest::new();

static EMBEDDED_FONT: &[u8] = include_bytes!("../tools/assets/zap-vga16.psf");
struct BootInfoStorage(core::cell::UnsafeCell<MaybeUninit<BootInfo>>);
unsafe impl Sync for BootInfoStorage {}
static BOOT_INFO: BootInfoStorage =
    BootInfoStorage(core::cell::UnsafeCell::new(MaybeUninit::uninit()));

struct ReservedRangeStorage(core::cell::UnsafeCell<[PhysicalRange; MAX_RESERVED_RANGES]>);
unsafe impl Sync for ReservedRangeStorage {}
static RESERVED_RANGES: ReservedRangeStorage = ReservedRangeStorage(core::cell::UnsafeCell::new(
    [PhysicalRange::EMPTY; MAX_RESERVED_RANGES],
));

struct Region {
    next: u64,
    end: u64,
}

struct PhysicalAllocator {
    regions: [Region; 128],
    count: usize,
}

impl PhysicalAllocator {
    fn new(entries: &[&Entry]) -> Self {
        let mut allocator = Self {
            regions: core::array::from_fn(|_| Region { next: 0, end: 0 }),
            count: 0,
        };
        for entry in entries {
            if entry.entry_type != EntryType::USABLE || allocator.count == allocator.regions.len() {
                continue;
            }
            let Some(end) = entry.base.checked_add(entry.length) else {
                continue;
            };
            let Some(aligned) = entry.base.checked_add(PAGE_SIZE - 1) else {
                continue;
            };
            let start = aligned & PAGE_MASK;
            let end = end & PAGE_MASK;
            if start < end {
                allocator.regions[allocator.count] = Region { next: start, end };
                allocator.count += 1;
            }
        }
        allocator
    }

    fn allocate_contiguous(&mut self, bytes: u64) -> Option<PhysicalRange> {
        let aligned_bytes = bytes.checked_add(PAGE_SIZE - 1)? & PAGE_MASK;
        for region in &mut self.regions[..self.count] {
            let end = region.next.checked_add(aligned_bytes)?;
            if end <= region.end {
                let range = PhysicalRange {
                    start: region.next,
                    length: aligned_bytes,
                };
                region.next = end;
                return Some(range);
            }
        }
        None
    }
}

struct PageTablePool {
    next: u64,
    end: u64,
    hhdm_offset: u64,
}

impl PageTablePool {
    fn allocate(&mut self) -> Option<u64> {
        if self.next >= self.end {
            return None;
        }
        let physical = self.next;
        self.next += PAGE_SIZE;
        let virtual_address = physical.checked_add(self.hhdm_offset)? as *mut u8;
        unsafe { ptr::write_bytes(virtual_address, 0, PAGE_SIZE as usize) };
        Some(physical)
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // This compile using the dzImage bootstrap, which is a custom bootloader that unpacks and loads the ImagineOS kernel 
    // from a compressed dzImage file. It initializes the serial port, framebuffer, and other necessary components before 
    // loading the kernel into memory and transferring control to it.
    let (tsc_start, tsc_frequency) = time::init();
    serial_init();
    framebuffer::init(
        FRAMEBUFFER_REQUEST
            .get_response()
            .and_then(|response| response.framebuffers().next()),
    );
    let _ = framebuffer::load_font(EMBEDDED_FONT);
    if !BASE_REVISION.is_supported() {
        fail("Limine protocol revision is unsupported");
    }
    let Some(memory_map) = MEMORY_MAP_REQUEST.get_response() else {
        fail("Limine did not provide a memory map");
    };
    let Some(hhdm) = HHDM_REQUEST.get_response() else {
        fail("Limine did not provide an HHDM mapping");
    };
    let Some(module_response) = MODULE_REQUEST.get_response() else {
        fail("Limine did not provide modules");
    };
    let Some(dz_module) = module_response
        .modules()
        .iter()
        .copied()
        .find(|module| module.path().to_bytes().ends_with(b"/dzImage"))
    else {
        fail("required /boot/dzImage module is missing");
    };

    let image = unsafe { core::slice::from_raw_parts(dz_module.addr(), dz_module.size() as usize) };
    let output_size = match dzimage::unpacked_size(image) {
        Ok(size) => size,
        Err(_) => fail("dzImage header is invalid"),
    };
    let entries = memory_map.entries();
    let mut allocator = PhysicalAllocator::new(entries);
    let Some(unpacked_range) = allocator.allocate_contiguous(output_size as u64) else {
        fail("not enough contiguous memory for the compressed kernel");
    };
    let unpacked_pointer = physical_pointer(unpacked_range.start, hhdm.offset());
    let unpacked = unsafe { core::slice::from_raw_parts_mut(unpacked_pointer, output_size) };
    serial_write(b"\r\n");
    framebuffer::write_char('\n');
    if dzimage::unpack(image, unpacked, show_progress).is_err() {
        fail("\r\nfailed to unpack or validate dzImage");
    }
    let kernel = unsafe { core::slice::from_raw_parts(unpacked_pointer, output_size) };

    let Some(table_range) =
        allocator.allocate_contiguous((PAGE_TABLE_POOL_PAGES as u64) * PAGE_SIZE)
    else {
        fail("\r\nnot enough memory for kernel page tables");
    };
    let mut page_tables = PageTablePool {
        next: table_range.start,
        end: table_range.start + table_range.length,
        hhdm_offset: hhdm.offset(),
    };
    let reserved = unsafe { &mut *RESERVED_RANGES.0.get() };
    let mut reserved_count = 0;
    let entry = match load_elf(
        kernel,
        &mut allocator,
        &mut page_tables,
        hhdm.offset(),
        reserved,
        &mut reserved_count,
    ) {
        Ok(entry) => entry,
        Err(_) => fail("\r\ncompressed payload is not a loadable ImagineOS ELF"),
    };
    if reserved_count == MAX_RESERVED_RANGES {
        fail("\r\nkernel uses too many memory ranges");
    }
    reserved[reserved_count] = table_range;
    reserved_count += 1;
    let mut boot_page_table_count = 0;
    if reserve_page_table_tree(
        read_cr3() & ADDRESS_MASK,
        4,
        hhdm.offset(),
        reserved,
        &mut reserved_count,
        &mut boot_page_table_count,
    )
    .is_err()
    {
        fail("\r\nLimine page-table tree exceeds reservation capacity");
    }
    reserved_count = sort_and_merge_ranges(&mut reserved[..reserved_count]);
    serial_write(b"Reserved Limine page-table pages: ");
    serial_write_number(boot_page_table_count as u64);
    serial_write(b"\n");

    let boot_info = BootInfo {
        memory_map: memory_map as *const MemoryMapResponse,
        framebuffer: FRAMEBUFFER_REQUEST
            .get_response()
            .map_or(ptr::null(), |response| {
                response as *const FramebufferResponse
            }),
        modules: module_response as *const ModuleResponse,
        rsdp_address: RSDP_REQUEST
            .get_response()
            .map_or(0, |response| response.address() as u64),
        hhdm_offset: hhdm.offset(),
        tsc_start,
        tsc_frequency,
        reserved_ranges: reserved.as_ptr(),
        reserved_count,
    };
    let boot_info_pointer = unsafe { (*BOOT_INFO.0.get()).write(boot_info) as *const BootInfo };
    let kernel_entry: extern "C" fn(*const BootInfo) -> ! =
        unsafe { core::mem::transmute(entry as usize) };
    kernel_entry(boot_info_pointer);
}

fn reserve_page_table_tree(
    table_physical: u64,
    level: u8,
    hhdm_offset: u64,
    reserved: &mut [PhysicalRange; MAX_RESERVED_RANGES],
    reserved_count: &mut usize,
    page_table_count: &mut usize,
) -> Result<(), ()> {
    // User address spaces share Limine's upper-half mappings.
    if reserved[..*reserved_count].iter().any(|range| {
        table_physical >= range.start
            && table_physical < range.start.checked_add(range.length).unwrap_or(u64::MAX)
    }) {
        return Ok(());
    }
    if *reserved_count == reserved.len() {
        return Err(());
    }
    reserved[*reserved_count] = PhysicalRange {
        start: table_physical,
        length: PAGE_SIZE,
    };
    *reserved_count += 1;
    *page_table_count += 1;

    if level == 1 {
        return Ok(());
    }
    let table = physical_pointer(table_physical, hhdm_offset).cast::<u64>();
    for index in 0..512 {
        let entry = unsafe { ptr::read_volatile(table.add(index)) };
        if entry & PRESENT == 0 || (level <= 3 && entry & HUGE_PAGE != 0) {
            continue;
        }
        reserve_page_table_tree(
            entry & ADDRESS_MASK,
            level - 1,
            hhdm_offset,
            reserved,
            reserved_count,
            page_table_count,
        )?;
    }
    Ok(())
}

fn sort_and_merge_ranges(ranges: &mut [PhysicalRange]) -> usize {
    for index in 1..ranges.len() {
        let mut position = index;
        while position > 0 && ranges[position].start < ranges[position - 1].start {
            ranges.swap(position, position - 1);
            position -= 1;
        }
    }

    let mut merged = 0;
    for index in 1..ranges.len() {
        let end = ranges[merged]
            .start
            .checked_add(ranges[merged].length)
            .unwrap_or(u64::MAX);
        let next_end = ranges[index]
            .start
            .checked_add(ranges[index].length)
            .unwrap_or(u64::MAX);
        if ranges[index].start <= end {
            ranges[merged].length = end.max(next_end) - ranges[merged].start;
        } else {
            merged += 1;
            ranges[merged] = ranges[index];
        }
    }
    merged + 1
}

#[derive(Clone, Copy)]
struct LoadSegment {
    virtual_address: u64,
    file_offset: usize,
    file_size: usize,
    memory_size: u64,
    flags: u32,
}

fn load_elf(
    elf: &[u8],
    allocator: &mut PhysicalAllocator,
    page_tables: &mut PageTablePool,
    hhdm_offset: u64,
    reserved: &mut [PhysicalRange; MAX_RESERVED_RANGES],
    reserved_count: &mut usize,
) -> Result<u64, ()> {
    if elf.get(..6) != Some(&[0x7f, b'E', b'L', b'F', 2, 1])
        || read_u16(elf, 18)? != 62
        || read_u16(elf, 16)? != 2
    {
        return Err(());
    }
    let entry = read_u64(elf, 24)?;
    let program_offset = usize::try_from(read_u64(elf, 32)?).map_err(|_| ())?;
    let entry_size = read_u16(elf, 54)? as usize;
    let count = read_u16(elf, 56)? as usize;
    if entry_size < 56 || count == 0 || count > MAX_PROGRAM_HEADERS {
        return Err(());
    }
    let table_end = program_offset
        .checked_add(entry_size.checked_mul(count).ok_or(())?)
        .filter(|end| *end <= elf.len())
        .ok_or(())?;
    let mut segments = [LoadSegment {
        virtual_address: 0,
        file_offset: 0,
        file_size: 0,
        memory_size: 0,
        flags: 0,
    }; MAX_LOAD_SEGMENTS];
    let mut segment_count = 0;
    for index in 0..count {
        let offset = program_offset + index * entry_size;
        if read_u32(elf, offset)? != 1 {
            continue;
        }
        if segment_count == MAX_LOAD_SEGMENTS {
            return Err(());
        }
        let file_offset = usize::try_from(read_u64(elf, offset + 8)?).map_err(|_| ())?;
        let virtual_address = read_u64(elf, offset + 16)?;
        let file_size = usize::try_from(read_u64(elf, offset + 32)?).map_err(|_| ())?;
        let memory_size = read_u64(elf, offset + 40)?;
        let file_end = file_offset
            .checked_add(file_size)
            .filter(|end| *end <= elf.len());
        if memory_size < file_size as u64 || file_end.is_none() || memory_size == 0 {
            return Err(());
        }
        virtual_address.checked_add(memory_size).ok_or(())?;
        segments[segment_count] = LoadSegment {
            virtual_address,
            file_offset,
            file_size,
            memory_size,
            flags: read_u32(elf, offset + 4)?,
        };
        segment_count += 1;
    }
    if segment_count == 0 {
        return Err(());
    }

    let mut entry_executable = false;
    for segment in &segments[..segment_count] {
        let page_offset = segment.virtual_address & (PAGE_SIZE - 1);
        let span = page_offset.checked_add(segment.memory_size).ok_or(())?;
        let allocated_length = span.checked_add(PAGE_SIZE - 1).ok_or(())? & PAGE_MASK;
        let Some(range) = allocator.allocate_contiguous(allocated_length) else {
            return Err(());
        };
        if *reserved_count == reserved.len() {
            return Err(());
        }
        reserved[*reserved_count] = range;
        *reserved_count += 1;
        let virtual_start = segment.virtual_address & PAGE_MASK;
        let physical_start = range.start;
        for page_offset in (0..allocated_length).step_by(PAGE_SIZE as usize) {
            map_page(
                virtual_start + page_offset,
                physical_start + page_offset,
                page_tables,
            )?;
        }
        let target = physical_pointer(range.start + page_offset, hhdm_offset);
        unsafe {
            ptr::write_bytes(target, 0, allocated_length as usize);
            ptr::copy_nonoverlapping(
                elf.as_ptr().add(segment.file_offset),
                target.add(page_offset as usize),
                segment.file_size,
            );
        }
        if segment.flags & 1 != 0
            && entry >= segment.virtual_address
            && entry < segment.virtual_address + segment.memory_size
        {
            entry_executable = true;
        }
    }
    let _ = table_end;
    if !entry_executable {
        return Err(());
    }
    flush_page_tables();
    Ok(entry)
}

fn map_page(
    virtual_address: u64,
    physical_address: u64,
    pool: &mut PageTablePool,
) -> Result<(), ()> {
    let indices = [
        ((virtual_address >> 39) & 0x1ff) as usize,
        ((virtual_address >> 30) & 0x1ff) as usize,
        ((virtual_address >> 21) & 0x1ff) as usize,
        ((virtual_address >> 12) & 0x1ff) as usize,
    ];
    let mut table_physical = read_cr3() & ADDRESS_MASK;
    for index in &indices[..3] {
        let table = physical_pointer(table_physical, pool.hhdm_offset).cast::<u64>();
        let slot = unsafe { table.add(*index) };
        let mut value = unsafe { ptr::read_volatile(slot) };
        if value & PRESENT == 0 {
            let Some(next_table) = pool.allocate() else {
                return Err(());
            };
            value = next_table | PRESENT | WRITABLE;
            unsafe { ptr::write_volatile(slot, value) };
        } else if value & HUGE_PAGE != 0 {
            return Err(());
        }
        table_physical = value & ADDRESS_MASK;
    }
    let table = physical_pointer(table_physical, pool.hhdm_offset).cast::<u64>();
    let slot = unsafe { table.add(indices[3]) };
    if unsafe { ptr::read_volatile(slot) } & PRESENT != 0 {
        return Err(());
    }
    unsafe { ptr::write_volatile(slot, physical_address | PRESENT | WRITABLE) };
    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, ()> {
    let value = bytes.get(offset..offset + 2).ok_or(())?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, ()> {
    let value = bytes.get(offset..offset + 4).ok_or(())?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, ()> {
    let value = bytes.get(offset..offset + 8).ok_or(())?;
    Ok(u64::from_le_bytes([
        value[0], value[1], value[2], value[3], value[4], value[5], value[6], value[7],
    ]))
}

fn physical_pointer(physical: u64, hhdm_offset: u64) -> *mut u8 {
    physical.wrapping_add(hhdm_offset) as *mut u8
}

fn read_cr3() -> u64 {
    let value: u64;
    unsafe { asm!("mov {}, cr3", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

fn flush_page_tables() {
    let root = read_cr3();
    unsafe { asm!("mov cr3, {}", in(reg) root, options(nostack, preserves_flags)) };
}

fn show_progress(percent: u8) {
    serial_write(b"\rUnpacking Kernel - ");
    serial_write_number(percent as u64);
    serial_write(b"%/100%");
    framebuffer::write_str("\rUnpacking Kernel - ");
    framebuffer_write_number(percent);
    framebuffer::write_str("%/100%");
    if percent == 100 {
        serial_write(b"\n");
        framebuffer::write_char('\n');
    }
}

fn framebuffer_write_number(value: u8) {
    if value >= 100 {
        framebuffer::write_str("100");
    } else if value >= 10 {
        framebuffer::write_char((b'0' + value / 10) as char);
        framebuffer::write_char((b'0' + value % 10) as char);
    } else {
        framebuffer::write_char((b'0' + value) as char);
    }
}

fn serial_write_number(mut value: u64) {
    let mut digits = [0u8; 20];
    let mut cursor = digits.len();
    if value == 0 {
        serial_write(b"0");
        return;
    }
    while value != 0 {
        cursor -= 1;
        digits[cursor] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    serial_write(&digits[cursor..]);
}

fn fail(message: &str) -> ! {
    serial_write(b"\r\nDZIMAGE BOOTSTRAP ERROR: ");
    serial_write(message.as_bytes());
    serial_write(b"\r\n");
    framebuffer::write_str("\r\nDZIMAGE BOOTSTRAP ERROR: ");
    framebuffer::write_str(message);
    framebuffer::write_char('\n');
    halt()
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
        out(0x3f8, byte);
    }
}

unsafe fn out(port: u16, value: u8) {
    asm!(
        "out dx, al",
        in("dx") port,
        in("al") value,
        options(nomem, nostack, preserves_flags)
    );
}

unsafe fn in_port(port: u16) -> u8 {
    let value: u8;
    asm!(
        "in al, dx",
        in("dx") port,
        out("al") value,
        options(nomem, nostack, preserves_flags)
    );
    value
}

fn halt() -> ! {
    loop {
        unsafe { asm!("cli; hlt", options(nomem, nostack)) };
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    fail("unrecoverable bootstrap failure")
}
