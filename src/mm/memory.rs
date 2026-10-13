use core::cell::UnsafeCell;
use limine::memory_map::{Entry, EntryType};

const PAGE_SIZE: u64 = 4096;
const MAX_REGIONS: usize = 128;

#[derive(Clone, Copy)]
struct Region {
    next: u64,
    end: u64,
}

impl Region {
    const EMPTY: Self = Self { next: 0, end: 0 };
}

struct FrameAllocator {
    regions: [Region; MAX_REGIONS],
    region_count: usize,
    current_region: usize,
    hhdm_offset: u64,
}

impl FrameAllocator {
    const fn new() -> Self {
        Self {
            regions: [Region::EMPTY; MAX_REGIONS],
            region_count: 0,
            current_region: 0,
            hhdm_offset: 0,
        }
    }
}

struct SharedAllocator(UnsafeCell<FrameAllocator>);
unsafe impl Sync for SharedAllocator {}

static ALLOCATOR: SharedAllocator = SharedAllocator(UnsafeCell::new(FrameAllocator::new()));

pub struct Frame {
    pub physical_address: u64,
    pub virtual_address: *mut u8,
}

pub fn init(entries: &[&Entry], hhdm_offset: u64) -> usize {
    let allocator = unsafe { &mut *ALLOCATOR.0.get() };
    allocator.regions = [Region::EMPTY; MAX_REGIONS];
    allocator.region_count = 0;
    allocator.current_region = 0;
    allocator.hhdm_offset = hhdm_offset;

    for entry in entries {
        if entry.entry_type != EntryType::USABLE || allocator.region_count == MAX_REGIONS {
            continue;
        }
        let Some(end) = entry.base.checked_add(entry.length) else {
            continue;
        };
        let Some(aligned_base) = entry.base.checked_add(PAGE_SIZE - 1) else {
            continue;
        };
        let base = aligned_base & !(PAGE_SIZE - 1);
        let end = end & !(PAGE_SIZE - 1);
        if base < end {
            let index = allocator.region_count;
            allocator.regions[index] = Region { next: base, end };
            allocator.region_count += 1;
        }
    }

    allocator
        .regions
        .iter()
        .take(allocator.region_count)
        .map(|region| ((region.end - region.next) / PAGE_SIZE) as usize)
        .sum()
}

pub fn allocate_frame() -> Option<Frame> {
    let allocator = unsafe { &mut *ALLOCATOR.0.get() };
    while allocator.current_region < allocator.region_count {
        let region = &mut allocator.regions[allocator.current_region];
        if region.next < region.end {
            let physical_address = region.next;
            // Checked: a region ending near u64::MAX must not wrap around
            // and hand out already-used frames.
            region.next = region.next.checked_add(PAGE_SIZE)?;
            let virtual_address = physical_address.checked_add(allocator.hhdm_offset)? as *mut u8;
            return Some(Frame {
                physical_address,
                virtual_address,
            });
        }
        allocator.current_region += 1;
    }
    None
}

pub fn phys_to_virt(physical_address: u64) -> Option<*mut u8> {
    let allocator = unsafe { &*ALLOCATOR.0.get() };
    physical_address
        .checked_add(allocator.hhdm_offset)
        .map(|virtual_address| virtual_address as *mut u8)
}
