use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::block::{BlockDevice, BlockError};

const ACPI_HEADER_SIZE: usize = 36;
const ACPI_TABLE_MAX_SIZE: usize = 1024 * 1024;
const ACPI_SCI_ENABLED: u16 = 1;
const ACPI_PM1_SLEEP_TYPE_MASK: u64 = 7 << 10;
const ACPI_PM1_SLEEP_ENABLE: u64 = 1 << 13;
const ACPI_ENABLE_POLL_LIMIT: usize = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AcpiRegister {
    address_space: u8,
    bit_width: u8,
    address: u64,
}

struct ShutdownMailbox(AtomicBool);

impl ShutdownMailbox {
    const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    fn send(&self) {
        self.0.store(true, Ordering::Release);
    }

    fn receive(&self) -> bool {
        self.0.swap(false, Ordering::AcqRel)
    }
}

static SHUTDOWN_MAILBOX: ShutdownMailbox = ShutdownMailbox::new();

#[derive(Clone, Copy)]
struct AcpiPowerRegisters {
    pm1a_control: Option<AcpiRegister>,
    pm1b_control: Option<AcpiRegister>,
    sleep_control: Option<AcpiRegister>,
    sleep_type_a: u16,
    sleep_type_b: u16,
    smi_command: u16,
    acpi_enable: u8,
    hhdm_offset: u64,
}

struct SharedAcpiPower(UnsafeCell<Option<AcpiPowerRegisters>>);
unsafe impl Sync for SharedAcpiPower {}
static ACPI_POWER: SharedAcpiPower = SharedAcpiPower(UnsafeCell::new(None));

pub fn request_shutdown() {
    SHUTDOWN_MAILBOX.send();
}

pub fn take_shutdown_request() -> bool {
    SHUTDOWN_MAILBOX.receive()
}

pub fn init(rsdp_address: u64, hhdm_offset: u64) {
    if rsdp_address == 0 {
        crate::console_write("ACPI RSDP was not provided by Limine\n");
        unsafe {
            *ACPI_POWER.0.get() = None;
        }
        return;
    }
    let registers = unsafe { discover_acpi_power_registers(rsdp_address, hhdm_offset) };
    unsafe {
        *ACPI_POWER.0.get() = registers;
    }
    if registers.is_some() {
        crate::console_write("ACPI S5 poweroff support ready\n");
    } else {
        crate::console_write("ACPI poweroff tables unavailable\n");
    }
}

pub fn power_off() -> Result<(), BlockError> {
    if crate::ata::is_ready() {
        crate::ata::PrimaryMaster.flush()?;
    }
    // Devolve a entrada e a tela ao console do kernel: se o desligamento
    // falhar, a sessão de texto volta a funcionar em vez de ficar muda porque o
    // servidor de display levou os eventos consigo.
    crate::input::release();
    crate::framebuffer::set_active(true);

    let Some(registers) = (unsafe { *ACPI_POWER.0.get() }) else {
        return Err(BlockError::NotReady);
    };
    crate::console_write("Sync complete; requesting ACPI poweroff\n");
    unsafe {
        if let Some(sleep_control) = registers.sleep_control {
            // Hardware-reduced sleep control is write-only, so it cannot be RMW.
            let value = u64::from((registers.sleep_type_a & 7) | (1 << 5));
            write_register(sleep_control, value, registers.hhdm_offset)
                .ok_or(BlockError::DeviceError)?;
        } else {
            let pm1a = registers.pm1a_control.ok_or(BlockError::DeviceError)?;
            if read_register(pm1a, registers.hhdm_offset).ok_or(BlockError::DeviceError)?
                & u64::from(ACPI_SCI_ENABLED)
                == 0
            {
                if registers.smi_command == 0 || registers.acpi_enable == 0 {
                    return Err(BlockError::DeviceError);
                }
                out_byte(registers.smi_command, registers.acpi_enable);
                let mut acpi_enabled = false;
                for _ in 0..ACPI_ENABLE_POLL_LIMIT {
                    if read_register(pm1a, registers.hhdm_offset).ok_or(BlockError::DeviceError)?
                        & u64::from(ACPI_SCI_ENABLED)
                        != 0
                    {
                        acpi_enabled = true;
                        break;
                    }
                    core::hint::spin_loop();
                }
                if !acpi_enabled {
                    return Err(BlockError::Timeout);
                }
            }

            if let Some(pm1b) = registers.pm1b_control {
                write_pm1_sleep(pm1b, registers.sleep_type_b, registers.hhdm_offset)
                    .ok_or(BlockError::DeviceError)?;
            }
            write_pm1_sleep(pm1a, registers.sleep_type_a, registers.hhdm_offset)
                .ok_or(BlockError::DeviceError)?;
        }
    }
    crate::console_write("ACPI S5 poweroff requested; halting CPU\n");
    loop {
        unsafe {
            asm!("cli; hlt", options(nomem, nostack));
        }
    }
}

unsafe fn out_byte(port: u16, value: u8) {
    asm!(
        "out dx, al",
        in("dx") port,
        in("al") value,
        options(nomem, nostack, preserves_flags)
    );
}

fn write_pm1_sleep(register: AcpiRegister, sleep_type: u16, hhdm_offset: u64) -> Option<()> {
    let previous = unsafe { read_register(register, hhdm_offset)? };
    let value = pm1_sleep_value(previous, sleep_type);
    unsafe { write_register(register, value, hhdm_offset) }
}

fn pm1_sleep_value(previous: u64, sleep_type: u16) -> u64 {
    // Retain SCI_EN and any other firmware-configured control bits.
    (previous & !(ACPI_PM1_SLEEP_TYPE_MASK | ACPI_PM1_SLEEP_ENABLE))
        | (u64::from(sleep_type & 7) << 10)
        | ACPI_PM1_SLEEP_ENABLE
}

unsafe fn discover_acpi_power_registers(
    rsdp_address: u64,
    hhdm_offset: u64,
) -> Option<AcpiPowerRegisters> {
    let Some(rsdp) = physical_bytes(rsdp_address, hhdm_offset, 36) else {
        crate::console_write("ACPI RSDP address is not mapped\n");
        return None;
    };
    if &rsdp[..8] != b"RSD PTR " || !checksum_valid(&rsdp[..20]) {
        crate::console_write("ACPI RSDP signature or checksum is invalid\n");
        return None;
    }

    let revision = rsdp[15];
    let (root_address, root_signature) = if revision >= 2 {
        let length = read_u32(&rsdp, 20)? as usize;
        if !(36..=4096).contains(&length) {
            return None;
        }
        let extended = physical_bytes(rsdp_address, hhdm_offset, length)?;
        if !checksum_valid(&extended) {
            return None;
        }
        let xsdt = read_u64(&extended, 24)?;
        if xsdt != 0 {
            (xsdt, b"XSDT" as &[u8])
        } else {
            (read_u32(&extended, 16)? as u64, b"RSDT" as &[u8])
        }
    } else {
        (read_u32(&rsdp, 16)? as u64, b"RSDT" as &[u8])
    };
    let Some(root) = acpi_table(root_address, hhdm_offset) else {
        crate::console_write("ACPI root table is invalid or unmapped\n");
        return None;
    };
    if &root[..4] != root_signature {
        crate::console_write("ACPI root table signature does not match RSDP\n");
        return None;
    }
    let entry_size = if root_signature == b"XSDT" { 8 } else { 4 };
    if (root.len() - ACPI_HEADER_SIZE) % entry_size != 0 {
        return None;
    }
    for entry in root[ACPI_HEADER_SIZE..].chunks_exact(entry_size) {
        let address = if entry_size == 8 {
            read_u64(entry, 0)?
        } else {
            read_u32(entry, 0)? as u64
        };
        let Some(fadt) = acpi_table(address, hhdm_offset) else {
            continue;
        };
        if &fadt[..4] == b"FACP" {
            let registers = parse_fadt_power_registers(&fadt, hhdm_offset);
            if registers.is_none() {
                crate::console_write("ACPI FADT has no supported S5/PM1 configuration\n");
            }
            return registers;
        }
    }
    crate::console_write("ACPI root table contains no valid FADT\n");
    None
}

unsafe fn parse_fadt_power_registers(fadt: &[u8], hhdm_offset: u64) -> Option<AcpiPowerRegisters> {
    let dsdt_address = if fadt.len() >= 148 {
        read_u64(fadt, 140)?.max(read_u32(fadt, 40)? as u64)
    } else {
        read_u32(fadt, 40)? as u64
    };
    let Some(dsdt) = acpi_table(dsdt_address, hhdm_offset) else {
        crate::console_write("ACPI DSDT is invalid or unmapped\n");
        return None;
    };
    if &dsdt[..4] != b"DSDT" {
        crate::console_write("ACPI DSDT address has the wrong signature\n");
        return None;
    }
    let Some((sleep_type_a, sleep_type_b)) = parse_s5_sleep_types(&dsdt[ACPI_HEADER_SIZE..]) else {
        crate::console_write("ACPI DSDT does not expose a supported _S5_ package\n");
        return None;
    };

    let flags = read_u32(fadt, 112).unwrap_or(0);
    let hardware_reduced = flags & (1 << 20) != 0;
    let (pm1a_control, pm1b_control, sleep_control, smi_command, acpi_enable) = if hardware_reduced
    {
        (None, None, parse_gas(fadt, 244, 8), 0, 0)
    } else {
        let control_length = fadt.get(89).copied().unwrap_or(2);
        let pm1a = parse_pm1_register(fadt, 64, 172, control_length);
        let pm1b = parse_pm1_register(fadt, 68, 184, control_length);
        if pm1a.is_none() {
            crate::console_write("ACPI FADT has no usable PM1a control register\n");
            return None;
        }
        let smi_command = u16::try_from(read_u32(fadt, 48)?).unwrap_or(0);
        (pm1a, pm1b, None, smi_command, fadt[52])
    };
    if hardware_reduced && sleep_control.is_none() {
        crate::console_write("ACPI FADT has no usable Sleep Control register\n");
        return None;
    }

    Some(AcpiPowerRegisters {
        pm1a_control,
        pm1b_control,
        sleep_control,
        sleep_type_a,
        sleep_type_b,
        smi_command,
        acpi_enable,
        hhdm_offset,
    })
}

unsafe fn acpi_table<'a>(address: u64, hhdm_offset: u64) -> Option<&'a [u8]> {
    let header = physical_bytes(address, hhdm_offset, ACPI_HEADER_SIZE)?;
    let length = read_u32(&header, 4)? as usize;
    if !(ACPI_HEADER_SIZE..=ACPI_TABLE_MAX_SIZE).contains(&length) {
        return None;
    }
    let table = physical_bytes(address, hhdm_offset, length)?;
    checksum_valid(&table).then_some(table)
}

unsafe fn physical_bytes<'a>(address: u64, hhdm_offset: u64, length: usize) -> Option<&'a [u8]> {
    if !crate::paging::map_hhdm_range(hhdm_offset, address, length) {
        return None;
    }
    let virtual_address = hhdm_offset.checked_add(address)?;
    let pointer = usize::try_from(virtual_address).ok()? as *const u8;
    Some(core::slice::from_raw_parts(pointer, length))
}

fn checksum_valid(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) == 0
}

fn parse_pm1_register(
    fadt: &[u8],
    legacy_offset: usize,
    gas_offset: usize,
    length: u8,
) -> Option<AcpiRegister> {
    if length >= 2 {
        if let Some(address) = read_u32(fadt, legacy_offset).map(u64::from) {
            if address != 0 {
                if let Some(register) = io_register(address, 16) {
                    return Some(register);
                }
            }
        }
    }
    parse_gas(fadt, gas_offset, 16)
}

fn parse_gas(fadt: &[u8], offset: usize, minimum_width: u8) -> Option<AcpiRegister> {
    let gas = fadt.get(offset..offset.checked_add(12)?)?;
    let address_space = gas[0];
    let bit_width = gas[1];
    let bit_offset = gas[2];
    let access_size = gas[3];
    let address = read_u64(gas, 4)?;
    if !matches!(address_space, 0 | 1)
        || bit_width < minimum_width
        || bit_offset != 0
        || address == 0
        || !matches!(access_size, 0..=4)
        || !gas_access_compatible(bit_width, access_size)
        || (address_space == 1 && io_register(address, bit_width).is_none())
        || (address_space == 0 && !matches!(bit_width, 8 | 16 | 32 | 64))
        || (address_space == 0 && address % u64::from(bit_width / 8) != 0)
    {
        return None;
    }
    Some(AcpiRegister {
        address_space,
        bit_width,
        address,
    })
}

fn gas_access_compatible(bit_width: u8, access_size: u8) -> bool {
    access_size == 0
        || matches!(
            (bit_width, access_size),
            (8, 1) | (16, 2) | (32, 3) | (64, 4)
        )
}

fn io_register(address: u64, bit_width: u8) -> Option<AcpiRegister> {
    if !matches!(bit_width, 8 | 16 | 32) {
        return None;
    }
    let port = u16::try_from(address).ok()?;
    let byte_width = u64::from(bit_width / 8);
    if port == 0 || address.checked_add(byte_width - 1)? > u16::MAX as u64 {
        return None;
    }
    Some(AcpiRegister {
        address_space: 1,
        bit_width,
        address,
    })
}

unsafe fn read_register(register: AcpiRegister, hhdm_offset: u64) -> Option<u64> {
    match register.address_space {
        1 => match register.bit_width {
            8 => {
                let value: u8;
                asm!(
                    "in al, dx",
                    in("dx") u16::try_from(register.address).ok()?,
                    out("al") value,
                    options(nomem, nostack, preserves_flags)
                );
                Some(u64::from(value))
            }
            16 => {
                let value: u16;
                asm!(
                    "in ax, dx",
                    in("dx") u16::try_from(register.address).ok()?,
                    out("ax") value,
                    options(nomem, nostack, preserves_flags)
                );
                Some(u64::from(value))
            }
            32 => {
                let value: u32;
                asm!(
                    "in eax, dx",
                    in("dx") u16::try_from(register.address).ok()?,
                    out("eax") value,
                    options(nomem, nostack, preserves_flags)
                );
                Some(u64::from(value))
            }
            _ => None,
        },
        0 => {
            let pointer = mmio_register_pointer(register, hhdm_offset)?;
            Some(match register.bit_width {
                8 => core::ptr::read_volatile(pointer.cast::<u8>()) as u64,
                16 => core::ptr::read_volatile(pointer.cast::<u16>()) as u64,
                32 => core::ptr::read_volatile(pointer.cast::<u32>()) as u64,
                64 => core::ptr::read_volatile(pointer.cast::<u64>()),
                _ => return None,
            })
        }
        _ => None,
    }
}

unsafe fn write_register(register: AcpiRegister, value: u64, hhdm_offset: u64) -> Option<()> {
    match register.address_space {
        1 => {
            let port = u16::try_from(register.address).ok()?;
            match register.bit_width {
                8 => asm!(
                    "out dx, al",
                    in("dx") port,
                    in("al") value as u8,
                    options(nomem, nostack, preserves_flags)
                ),
                16 => asm!(
                    "out dx, ax",
                    in("dx") port,
                    in("ax") value as u16,
                    options(nomem, nostack, preserves_flags)
                ),
                32 => asm!(
                    "out dx, eax",
                    in("dx") port,
                    in("eax") value as u32,
                    options(nomem, nostack, preserves_flags)
                ),
                _ => return None,
            }
            Some(())
        }
        0 => {
            let pointer = mmio_register_pointer(register, hhdm_offset)?;
            match register.bit_width {
                8 => core::ptr::write_volatile(pointer.cast::<u8>(), value as u8),
                16 => core::ptr::write_volatile(pointer.cast::<u16>(), value as u16),
                32 => core::ptr::write_volatile(pointer.cast::<u32>(), value as u32),
                64 => core::ptr::write_volatile(pointer.cast::<u64>(), value),
                _ => return None,
            }
            Some(())
        }
        _ => None,
    }
}

fn mmio_register_pointer(register: AcpiRegister, hhdm_offset: u64) -> Option<*mut u8> {
    if register.address_space != 0 {
        return None;
    }
    let byte_width = usize::from(register.bit_width).div_ceil(8);
    if register.address % byte_width as u64 != 0 {
        return None;
    }
    if !crate::paging::map_hhdm_range(hhdm_offset, register.address, byte_width) {
        return None;
    }
    let virtual_address = hhdm_offset.checked_add(register.address)?;
    Some(usize::try_from(virtual_address).ok()? as *mut u8)
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn parse_s5_sleep_types(aml: &[u8]) -> Option<(u16, u16)> {
    for start in 0..aml.len().saturating_sub(4) {
        let name_length = if aml[start..].starts_with(&[0x08, 0x5c, b'_', b'S', b'5', b'_']) {
            6
        } else if aml[start..].starts_with(&[0x08, b'_', b'S', b'5', b'_']) {
            5
        } else {
            continue;
        };
        let package = start + name_length;
        if aml.get(package) != Some(&0x12) {
            continue;
        }
        let Some(package_bytes) = aml.get(package + 1..) else {
            continue;
        };
        let Some((_, length_bytes)) = aml_package_length(package_bytes) else {
            continue;
        };
        let elements = package + 1 + length_bytes;
        if aml.get(elements).map_or(true, |count| *count < 2) {
            continue;
        }
        let Some(first_element) = aml.get(elements + 1..) else {
            continue;
        };
        let Some((sleep_type_a, a_length)) = aml_integer(first_element) else {
            continue;
        };
        let Some(second_element) = aml.get(elements + 1 + a_length..) else {
            continue;
        };
        let Some((sleep_type_b, _)) = aml_integer(second_element) else {
            continue;
        };
        if sleep_type_a <= 7 && sleep_type_b <= 7 {
            return Some((sleep_type_a, sleep_type_b));
        }
    }
    None
}

fn aml_package_length(bytes: &[u8]) -> Option<(usize, usize)> {
    let lead = *bytes.first()?;
    let following = (lead >> 6) as usize;
    if following == 0 {
        return Some(((lead & 0x3f) as usize, 1));
    }
    if bytes.len() <= following {
        return None;
    }
    let mut length = (lead & 0x0f) as usize;
    for index in 0..following {
        length |= (bytes[index + 1] as usize) << (4 + index * 8);
    }
    Some((length, following + 1))
}

fn aml_integer(bytes: &[u8]) -> Option<(u16, usize)> {
    match *bytes.first()? {
        0x00 => Some((0, 1)),
        0x01 => Some((1, 1)),
        0x0a => Some((*bytes.get(1)? as u16, 2)),
        0x0b => Some((u16::from_le_bytes(bytes.get(1..3)?.try_into().ok()?), 3)),
        0x0c => {
            let value = read_u32(bytes, 1)?;
            Some((u16::try_from(value).ok()?, 5))
        }
        _ => None,
    }
}
