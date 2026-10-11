#![allow(dead_code)]
use core::arch::asm;
use core::cell::UnsafeCell;

use crate::block::{BlockDevice, BlockError, SECTOR_SIZE};
use crate::{ata, gpt, ramfs};

const ESP_START: u64 = 2048;
const ESP_SECTORS: u64 = 262_144;
const ESP_ROOT_ENTRIES: usize = 512;
const ESP_ROOT_SECTORS: u64 = (ESP_ROOT_ENTRIES * 32 / SECTOR_SIZE) as u64;
const SECTORS_PER_CLUSTER: u64 = 8;
const MAX_FAT_SECTORS: usize = 256;
const GPT_ENTRY_COUNT: usize = 128;
const GPT_ENTRY_SIZE: usize = 128;
const GPT_ENTRY_SECTORS: u64 = (GPT_ENTRY_COUNT * GPT_ENTRY_SIZE / SECTOR_SIZE) as u64;
const EFI_SYSTEM_PARTITION_GUID: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];
const MIN_DISK_SECTORS: u64 = ESP_START + ESP_SECTORS + 4096;
const FAT_EOC: u16 = 0xffff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallError {
    NoDisk,
    DiskTooSmall,
    MissingPayload,
    PayloadTooLarge,
    DfsFormat,
    InvalidAccountConfig,
    EntropyUnavailable,
    Block(BlockError),
}

struct SharedBuffer<T>(UnsafeCell<T>);
unsafe impl<T> Sync for SharedBuffer<T> {}

static GPT_ENTRIES: SharedBuffer<[[u8; SECTOR_SIZE]; GPT_ENTRY_SECTORS as usize]> = SharedBuffer(
    UnsafeCell::new([[0; SECTOR_SIZE]; GPT_ENTRY_SECTORS as usize]),
);
static FAT_TABLE: SharedBuffer<[u8; MAX_FAT_SECTORS * SECTOR_SIZE]> =
    SharedBuffer(UnsafeCell::new([0; MAX_FAT_SECTORS * SECTOR_SIZE]));

#[derive(Clone, Copy)]
struct DirectoryEntry {
    name: [u8; 11],
    attributes: u8,
    cluster: u16,
    size: u32,
}

struct FileToInstall<'a> {
    name: [u8; 11],
    long_name: &'static str,
    contents: &'a [u8],
    first_cluster: u16,
    cluster_count: u16,
}

pub fn install_primary_master(
    account_config: &imagineos_abi::InstallConfig,
) -> Result<(), InstallError> {
    let device = ata::PrimaryMaster;
    if device.sector_count() == 0 {
        return Err(InstallError::NoDisk);
    }
    let bootstrap =
        ramfs::read("/system/install/bootstrap.elf").ok_or(InstallError::MissingPayload)?;
    let dz_image = ramfs::read("/system/install/dzImage").ok_or(InstallError::MissingPayload)?;
    let efi = ramfs::read("/system/install/BOOTX64.EFI").ok_or(InstallError::MissingPayload)?;
    let config = ramfs::read("/system/install/limine.conf").ok_or(InstallError::MissingPayload)?;
    let startup = ramfs::read("/system/install/startup.nsh").ok_or(InstallError::MissingPayload)?;
    let ramfs_image =
        ramfs::read("/system/install/initramfs-installed.tar").ok_or(InstallError::MissingPayload)?;
    install_configured(
        &device,
        bootstrap,
        dz_image,
        ramfs_image,
        efi,
        config,
        startup,
        Some(account_config),
    )
}

pub fn install(
    device: &impl BlockDevice,
    bootstrap: &[u8],
    dz_image: &[u8],
    ramfs_image: &[u8],
    efi: &[u8],
    config: &[u8],
    startup: &[u8],
) -> Result<(), InstallError> {
    install_configured(
        device,
        bootstrap,
        dz_image,
        ramfs_image,
        efi,
        config,
        startup,
        None,
    )
}

fn install_configured(
    device: &impl BlockDevice,
    bootstrap: &[u8],
    dz_image: &[u8],
    ramfs_image: &[u8],
    efi: &[u8],
    config: &[u8],
    startup: &[u8],
    account_config: Option<&imagineos_abi::InstallConfig>,
) -> Result<(), InstallError> {
    if let Some(config) = account_config {
        crate::accounts::validate_install_config(config)
            .map_err(|_| InstallError::InvalidAccountConfig)?;
    }
    let account_database = match account_config {
        Some(config) if config.add_user != 0 => Some(
            crate::accounts::make_database(config).map_err(|_| InstallError::EntropyUnavailable)?,
        ),
        _ => None,
    };
    if device.sector_count() < MIN_DISK_SECTORS {
        return Err(InstallError::DiskTooSmall);
    }
    if bootstrap.is_empty()
        || dz_image.is_empty()
        || efi.is_empty()
        || config.is_empty()
        || startup.is_empty()
        || ramfs_image.is_empty()
    {
        return Err(InstallError::MissingPayload);
    }

    let mut files = [
        FileToInstall {
            name: *b"LIMINE  CNF",
            long_name: "limine.conf",
            contents: config,
            first_cluster: 0,
            cluster_count: 0,
        },
        FileToInstall {
            name: *b"BOOTX64 EFI",
            long_name: "BOOTX64.EFI",
            contents: efi,
            first_cluster: 0,
            cluster_count: 0,
        },
        FileToInstall {
            name: *b"BOOT    ELF",
            long_name: "bootstrap.elf",
            contents: bootstrap,
            first_cluster: 0,
            cluster_count: 0,
        },
        FileToInstall {
            name: *b"DZIMAGE    ",
            long_name: "dzImage",
            contents: dz_image,
            first_cluster: 0,
            cluster_count: 0,
        },
        FileToInstall {
            name: *b"INITRAM IMG",
            long_name: "initramfs.img",
            contents: ramfs_image,
            first_cluster: 0,
            cluster_count: 0,
        },
        FileToInstall {
            name: *b"STARTUP NSH",
            long_name: "startup.nsh",
            contents: startup,
            first_cluster: 0,
            cluster_count: 0,
        },
    ];

    let fat_sectors = calculate_fat_sectors(&files)?;
    let data_start = ESP_START + 1 + fat_sectors as u64 * 2 + ESP_ROOT_SECTORS;
    let cluster_capacity = (ESP_SECTORS - (data_start - ESP_START)) / SECTORS_PER_CLUSTER;
    let mut next_cluster = 5u32;
    for file in &mut files {
        u32::try_from(file.contents.len()).map_err(|_| InstallError::PayloadTooLarge)?;
        let clusters = (file.contents.len() as u64)
            .checked_add(SECTORS_PER_CLUSTER * SECTOR_SIZE as u64 - 1)
            .ok_or(InstallError::PayloadTooLarge)?
            / (SECTORS_PER_CLUSTER * SECTOR_SIZE as u64);
        if clusters == 0 || next_cluster as u64 + clusters > cluster_capacity + 2 {
            return Err(InstallError::PayloadTooLarge);
        }
        file.first_cluster = next_cluster as u16;
        file.cluster_count = clusters as u16;
        next_cluster += clusters as u32;
    }

    write_gpt(device)?;
    format_fat16(device, fat_sectors, data_start, &mut files)?;
    let gpt = gpt::Gpt::read_primary(device).map_err(|_| InstallError::DfsFormat)?;
    let partition = gpt
        .find_partition(device, &gpt::DFS_PARTITION_TYPE_GUID)
        .map_err(|_| InstallError::DfsFormat)?;
    let dfs = super::dfs::Dfs::format(
        device,
        partition.first_lba,
        partition.last_lba - partition.first_lba + 1,
    )
    .map_err(|_| InstallError::DfsFormat)?;
    dfs.seed_from_ustar(device, ramfs_image)
        .map_err(|_| InstallError::DfsFormat)?;
    if let Some(config) = account_config {
        match dfs.create_directory(device, "etc", 0o755, 0, 0) {
            Ok(()) | Err(super::dfs::DfsError::AlreadyExists) => {}
            Err(_) => return Err(InstallError::DfsFormat),
        }
        let hostname = crate::accounts::hostname(config);
        dfs.write_file(device, "etc/hostname", 0, hostname, true, 0o644, 0, 0)
            .map_err(|_| InstallError::DfsFormat)?;
        if let Some(database) = account_database {
            dfs.write_file(device, "etc/users.db", 0, &database, true, 0o600, 0, 0)
                .map_err(|_| InstallError::DfsFormat)?;
            let mut home_path = [0u8; 40];
            home_path[..5].copy_from_slice(b"home/");
            let home_length = 5 + config.username_length as usize;
            home_path[5..home_length]
                .copy_from_slice(&config.username[..config.username_length as usize]);
            let home_path = core::str::from_utf8(&home_path[..home_length])
                .map_err(|_| InstallError::InvalidAccountConfig)?;
            match dfs.create_directory(device, home_path, 0o750, 1000, 1000) {
                Ok(()) | Err(super::dfs::DfsError::AlreadyExists) => {}
                Err(_) => return Err(InstallError::DfsFormat),
            }
        }
    }
    device.flush().map_err(InstallError::Block)?;
    Ok(())
}

fn calculate_fat_sectors(files: &[FileToInstall<'_>]) -> Result<usize, InstallError> {
    let mut fat_sectors = 1u64;
    loop {
        let data_sectors = ESP_SECTORS - 1 - fat_sectors * 2 - ESP_ROOT_SECTORS;
        let clusters = data_sectors / SECTORS_PER_CLUSTER;
        let needed = ((clusters + 2) * 2).div_ceil(SECTOR_SIZE as u64);
        if needed == fat_sectors {
            break;
        }
        fat_sectors = needed;
    }
    let fat_sectors = fat_sectors as usize;
    if fat_sectors == 0 || fat_sectors > MAX_FAT_SECTORS {
        return Err(InstallError::PayloadTooLarge);
    }
    let data_sectors = ESP_SECTORS - 1 - fat_sectors as u64 * 2 - ESP_ROOT_SECTORS;
    let data_clusters = data_sectors / SECTORS_PER_CLUSTER;
    let used_clusters = files
        .iter()
        .try_fold(3u64, |total, file| {
            let count = (file.contents.len() as u64)
                .checked_add(SECTORS_PER_CLUSTER * SECTOR_SIZE as u64 - 1)?
                / (SECTORS_PER_CLUSTER * SECTOR_SIZE as u64);
            total.checked_add(count)
        })
        .ok_or(InstallError::PayloadTooLarge)?;
    if !(4085..65525).contains(&data_clusters) || used_clusters > data_clusters {
        return Err(InstallError::PayloadTooLarge);
    }
    Ok(fat_sectors)
}

fn write_gpt(device: &impl BlockDevice) -> Result<(), InstallError> {
    let total_sectors = device.sector_count();
    let backup_header_lba = total_sectors - 1;
    let backup_entries_lba = backup_header_lba - GPT_ENTRY_SECTORS;
    let last_usable_lba = backup_entries_lba - 1;
    let dfs_end_lba = last_usable_lba - ((last_usable_lba + 1) % 2048);
    let dfs_start_lba = ESP_START + ESP_SECTORS;
    if dfs_end_lba <= dfs_start_lba {
        return Err(InstallError::DiskTooSmall);
    }

    let entries = unsafe { &mut *GPT_ENTRIES.0.get() };
    entries.fill([0; SECTOR_SIZE]);
    let raw_entries = unsafe {
        core::slice::from_raw_parts_mut(
            entries.as_mut_ptr().cast::<u8>(),
            GPT_ENTRY_SECTORS as usize * SECTOR_SIZE,
        )
    };
    let disk_guid = unique_guid();
    write_partition_entry(
        raw_entries,
        0,
        &EFI_SYSTEM_PARTITION_GUID,
        &unique_guid(),
        ESP_START,
        ESP_START + ESP_SECTORS - 1,
        b"IMAGINEOS_ESP",
    );
    write_partition_entry(
        raw_entries,
        1,
        &gpt::DFS_PARTITION_TYPE_GUID,
        &unique_guid(),
        dfs_start_lba,
        dfs_end_lba,
        b"DREAMCORE_DFS",
    );
    let entries_crc = crc32(raw_entries);

    for index in 0..GPT_ENTRY_SECTORS {
        device
            .write_sector(backup_entries_lba + index, &entries[index as usize])
            .map_err(InstallError::Block)?;
    }
    let backup_header = make_gpt_header(
        backup_header_lba,
        1,
        34,
        last_usable_lba,
        backup_entries_lba,
        entries_crc,
        &disk_guid,
    );
    device
        .write_sector(backup_header_lba, &backup_header)
        .map_err(InstallError::Block)?;

    for index in 0..GPT_ENTRY_SECTORS {
        device
            .write_sector(2 + index, &entries[index as usize])
            .map_err(InstallError::Block)?;
    }
    let primary_header = make_gpt_header(
        1,
        backup_header_lba,
        34,
        last_usable_lba,
        2,
        entries_crc,
        &disk_guid,
    );
    device
        .write_sector(1, &primary_header)
        .map_err(InstallError::Block)?;

    let mut protective_mbr = [0; SECTOR_SIZE];
    protective_mbr[446 + 4] = 0xee;
    protective_mbr[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
    protective_mbr[446 + 12..446 + 16]
        .copy_from_slice(&((total_sectors - 1).min(u32::MAX as u64) as u32).to_le_bytes());
    protective_mbr[510..512].copy_from_slice(&[0x55, 0xaa]);
    device
        .write_sector(0, &protective_mbr)
        .map_err(InstallError::Block)?;
    Ok(())
}

fn write_partition_entry(
    entries: &mut [u8],
    index: usize,
    type_guid: &[u8; 16],
    unique_guid: &[u8; 16],
    first_lba: u64,
    last_lba: u64,
    name: &[u8],
) {
    let start = index * GPT_ENTRY_SIZE;
    let entry = &mut entries[start..start + GPT_ENTRY_SIZE];
    entry[..16].copy_from_slice(type_guid);
    entry[16..32].copy_from_slice(unique_guid);
    entry[32..40].copy_from_slice(&first_lba.to_le_bytes());
    entry[40..48].copy_from_slice(&last_lba.to_le_bytes());
    for (index, byte) in name.iter().copied().take(36).enumerate() {
        entry[56 + index * 2] = byte;
    }
}

fn format_fat16(
    device: &impl BlockDevice,
    fat_sectors: usize,
    data_start: u64,
    files: &mut [FileToInstall<'_>],
) -> Result<(), InstallError> {
    let fat = unsafe { &mut *FAT_TABLE.0.get() };
    fat.fill(0);
    fat[..4].copy_from_slice(&[0xf8, 0xff, 0xff, 0xff]);
    for directory_cluster in 2..=4 {
        set_fat_entry(fat, directory_cluster, FAT_EOC);
    }
    for file in files.iter() {
        for offset in 0..file.cluster_count {
            let cluster = file.first_cluster + offset;
            let next = if offset + 1 == file.cluster_count {
                FAT_EOC
            } else {
                cluster + 1
            };
            set_fat_entry(fat, cluster, next);
        }
    }

    let mut boot = [0; SECTOR_SIZE];
    boot[0..3].copy_from_slice(&[0xeb, 0x3c, 0x90]);
    boot[3..11].copy_from_slice(b"IMAGINE ");
    boot[11..13].copy_from_slice(&(SECTOR_SIZE as u16).to_le_bytes());
    boot[13] = SECTORS_PER_CLUSTER as u8;
    boot[14..16].copy_from_slice(&1u16.to_le_bytes());
    boot[16] = 2;
    boot[17..19].copy_from_slice(&(ESP_ROOT_ENTRIES as u16).to_le_bytes());
    boot[19..21].copy_from_slice(&0u16.to_le_bytes());
    boot[21] = 0xf8;
    boot[22..24].copy_from_slice(&(fat_sectors as u16).to_le_bytes());
    boot[24..26].copy_from_slice(&63u16.to_le_bytes());
    boot[26..28].copy_from_slice(&255u16.to_le_bytes());
    boot[28..32].copy_from_slice(&(ESP_START as u32).to_le_bytes());
    boot[32..36].copy_from_slice(&(ESP_SECTORS as u32).to_le_bytes());
    boot[36] = 0x80;
    boot[38] = 0x29;
    boot[39..43].copy_from_slice(&(read_tsc() as u32).to_le_bytes());
    boot[43..54].copy_from_slice(b"IMAGINEOS  ");
    boot[54..62].copy_from_slice(b"FAT16   ");
    boot[510..512].copy_from_slice(&[0x55, 0xaa]);
    device
        .write_sector(ESP_START, &boot)
        .map_err(InstallError::Block)?;

    let mut sector = [0; SECTOR_SIZE];
    for index in 0..fat_sectors {
        sector.copy_from_slice(&fat[index * SECTOR_SIZE..(index + 1) * SECTOR_SIZE]);
        device
            .write_sector(ESP_START + 1 + index as u64, &sector)
            .map_err(InstallError::Block)?;
        device
            .write_sector(ESP_START + 1 + fat_sectors as u64 + index as u64, &sector)
            .map_err(InstallError::Block)?;
    }

    let root_start = ESP_START + 1 + fat_sectors as u64 * 2;
    let efi_dir = DirectoryEntry {
        name: *b"EFI        ",
        attributes: 0x10,
        cluster: 2,
        size: 0,
    };
    let boot_dir = DirectoryEntry {
        name: *b"BOOT       ",
        attributes: 0x10,
        cluster: 4,
        size: 0,
    };
    write_directory_with_files(
        device,
        root_start,
        ESP_ROOT_SECTORS as usize,
        &[efi_dir, boot_dir],
        &[&files[0], &files[5]],
    )?;

    let efi_cluster_lba = data_start;
    let system_boot_cluster_lba = data_start + SECTORS_PER_CLUSTER * 2;
    let efi_entries = [
        dot_entry(b".          ", 2),
        dot_entry(b"..         ", 0),
        DirectoryEntry {
            name: *b"BOOT       ",
            attributes: 0x10,
            cluster: 3,
            size: 0,
        },
    ];
    let efi_boot_entries = [dot_entry(b".          ", 3), dot_entry(b"..         ", 2)];
    let system_boot_entries = [dot_entry(b".          ", 4), dot_entry(b"..         ", 0)];
    write_directory(
        device,
        efi_cluster_lba,
        SECTORS_PER_CLUSTER as usize,
        &efi_entries,
    )?;
    write_directory_with_files(
        device,
        data_start + SECTORS_PER_CLUSTER,
        SECTORS_PER_CLUSTER as usize,
        &efi_boot_entries,
        &[&files[1]],
    )?;
    write_directory_with_files(
        device,
        system_boot_cluster_lba,
        SECTORS_PER_CLUSTER as usize,
        &system_boot_entries,
        &[&files[2], &files[3], &files[4]],
    )?;

    for file in files {
        write_file_data(device, data_start, file)?;
    }
    Ok(())
}

fn dot_entry(name: &[u8; 11], cluster: u16) -> DirectoryEntry {
    DirectoryEntry {
        name: *name,
        attributes: 0x10,
        cluster,
        size: 0,
    }
}

fn directory_entry_for(file: &FileToInstall<'_>) -> DirectoryEntry {
    DirectoryEntry {
        name: file.name,
        attributes: 0x20,
        cluster: file.first_cluster,
        size: file.contents.len() as u32,
    }
}

fn write_directory(
    device: &impl BlockDevice,
    first_lba: u64,
    sector_count: usize,
    entries: &[DirectoryEntry],
) -> Result<(), InstallError> {
    for sector_index in 0..sector_count {
        let mut sector = [0; SECTOR_SIZE];
        for (entry_index, entry) in entries.iter().enumerate() {
            if entry_index / 16 == sector_index {
                write_directory_entry(&mut sector, entry_index % 16 * 32, entry);
            }
        }
        device
            .write_sector(first_lba + sector_index as u64, &sector)
            .map_err(InstallError::Block)?;
    }
    Ok(())
}

fn write_directory_with_files(
    device: &impl BlockDevice,
    first_lba: u64,
    sector_count: usize,
    entries: &[DirectoryEntry],
    files: &[&FileToInstall<'_>],
) -> Result<(), InstallError> {
    let entry_count = entries.len() + files.len() * 2;
    if entry_count > sector_count * 16 || files.iter().any(|file| file.long_name.len() > 13) {
        return Err(InstallError::PayloadTooLarge);
    }
    for sector_index in 0..sector_count {
        let mut sector = [0; SECTOR_SIZE];
        for slot in 0..16 {
            let global_index = sector_index * 16 + slot;
            if global_index >= entry_count {
                break;
            }
            let offset = slot * 32;
            if global_index < entries.len() {
                write_directory_entry(&mut sector, offset, &entries[global_index]);
                continue;
            }
            let file_index = (global_index - entries.len()) / 2;
            let file = files[file_index];
            if (global_index - entries.len()) % 2 == 0 {
                write_lfn_entry(&mut sector, offset, file.long_name, &file.name);
            } else {
                write_directory_entry(&mut sector, offset, &directory_entry_for(file));
            }
        }
        device
            .write_sector(first_lba + sector_index as u64, &sector)
            .map_err(InstallError::Block)?;
    }
    Ok(())
}

fn write_directory_entry(sector: &mut [u8; SECTOR_SIZE], offset: usize, entry: &DirectoryEntry) {
    sector[offset..offset + 11].copy_from_slice(&entry.name);
    sector[offset + 11] = entry.attributes;
    sector[offset + 20..offset + 22].fill(0);
    sector[offset + 26..offset + 28].copy_from_slice(&entry.cluster.to_le_bytes());
    sector[offset + 28..offset + 32].copy_from_slice(&entry.size.to_le_bytes());
}

fn write_lfn_entry(
    sector: &mut [u8; SECTOR_SIZE],
    offset: usize,
    name: &str,
    short_name: &[u8; 11],
) {
    let entry = &mut sector[offset..offset + 32];
    entry[0] = 0x40 | name.len().div_ceil(13) as u8;
    entry[11] = 0x0f;
    entry[13] = short_name_checksum(short_name);
    let mut units = [0xffffu16; 13];
    for (unit, byte) in units.iter_mut().zip(name.bytes()) {
        *unit = byte as u16;
    }
    if name.len() < units.len() {
        units[name.len()] = 0;
    }
    for (index, unit) in units.iter().take(5).enumerate() {
        let start = 1 + index * 2;
        entry[start..start + 2].copy_from_slice(&unit.to_le_bytes());
    }
    for (index, unit) in units.iter().skip(5).take(6).enumerate() {
        let start = 14 + index * 2;
        entry[start..start + 2].copy_from_slice(&unit.to_le_bytes());
    }
    for (index, unit) in units.iter().skip(11).enumerate() {
        let start = 28 + index * 2;
        entry[start..start + 2].copy_from_slice(&unit.to_le_bytes());
    }
}

fn short_name_checksum(name: &[u8; 11]) -> u8 {
    name.iter()
        .fold(0u8, |sum, byte| sum.rotate_right(1).wrapping_add(*byte))
}

fn write_file_data(
    device: &impl BlockDevice,
    data_start: u64,
    file: &FileToInstall<'_>,
) -> Result<(), InstallError> {
    let mut copied = 0usize;
    for cluster_offset in 0..file.cluster_count as u64 {
        let cluster = file.first_cluster as u64 + cluster_offset;
        let cluster_lba = data_start + (cluster - 2) * SECTORS_PER_CLUSTER;
        for sector_offset in 0..SECTORS_PER_CLUSTER {
            let mut sector = [0; SECTOR_SIZE];
            let remaining = file.contents.len().saturating_sub(copied);
            let count = remaining.min(SECTOR_SIZE);
            if count != 0 {
                sector[..count].copy_from_slice(&file.contents[copied..copied + count]);
                copied += count;
            }
            device
                .write_sector(cluster_lba + sector_offset, &sector)
                .map_err(InstallError::Block)?;
        }
    }
    Ok(())
}

fn set_fat_entry(fat: &mut [u8], cluster: u16, next: u16) {
    let offset = cluster as usize * 2;
    fat[offset..offset + 2].copy_from_slice(&next.to_le_bytes());
}

fn make_gpt_header(
    current_lba: u64,
    backup_lba: u64,
    first_usable_lba: u64,
    last_usable_lba: u64,
    entries_lba: u64,
    entries_crc: u32,
    disk_guid: &[u8; 16],
) -> [u8; SECTOR_SIZE] {
    let mut header = [0; SECTOR_SIZE];
    header[..8].copy_from_slice(b"EFI PART");
    header[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    header[12..16].copy_from_slice(&92u32.to_le_bytes());
    header[24..32].copy_from_slice(&current_lba.to_le_bytes());
    header[32..40].copy_from_slice(&backup_lba.to_le_bytes());
    header[40..48].copy_from_slice(&first_usable_lba.to_le_bytes());
    header[48..56].copy_from_slice(&last_usable_lba.to_le_bytes());
    header[56..72].copy_from_slice(disk_guid);
    header[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    header[80..84].copy_from_slice(&(GPT_ENTRY_COUNT as u32).to_le_bytes());
    header[84..88].copy_from_slice(&(GPT_ENTRY_SIZE as u32).to_le_bytes());
    header[88..92].copy_from_slice(&entries_crc.to_le_bytes());
    let checksum = crc32(&header[..92]);
    header[16..20].copy_from_slice(&checksum.to_le_bytes());
    header
}

fn unique_guid() -> [u8; 16] {
    let value = read_tsc();
    let mut guid = [0; 16];
    guid[..8].copy_from_slice(&value.to_le_bytes());
    guid[8..16].copy_from_slice(&value.rotate_left(23).to_le_bytes());
    guid[7] = (guid[7] & 0x0f) | 0x40;
    guid[8] = (guid[8] & 0x3f) | 0x80;
    guid
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fat_geometry_fits_the_reserved_esp() {
        let files = [
            FileToInstall {
                name: *b"LIMINE  CNF",
                long_name: "limine.conf",
                contents: b"boot",
                first_cluster: 0,
                cluster_count: 0,
            },
            FileToInstall {
                name: *b"BOOTX64 EFI",
                long_name: "BOOTX64.EFI",
                contents: b"efi",
                first_cluster: 0,
                cluster_count: 0,
            },
            FileToInstall {
                name: *b"BOOT    ELF",
                long_name: "bootstrap.elf",
                contents: b"bootstrap",
                first_cluster: 0,
                cluster_count: 0,
            },
            FileToInstall {
                name: *b"DZIMAGE    ",
                long_name: "dzImage",
                contents: b"dzimage",
                first_cluster: 0,
                cluster_count: 0,
            },
            FileToInstall {
                name: *b"INITRAM IMG",
                long_name: "initramfs.img",
                contents: b"initramfs",
                first_cluster: 0,
                cluster_count: 0,
            },
            FileToInstall {
                name: *b"STARTUP NSH",
                long_name: "startup.nsh",
                contents: b"startup",
                first_cluster: 0,
                cluster_count: 0,
            },
        ];
        assert_eq!(calculate_fat_sectors(&files), Ok(128));
    }

    #[test]
    fn calculates_fat16_cluster_chain() {
        let mut fat = [0; 16];
        set_fat_entry(&mut fat, 2, 3);
        set_fat_entry(&mut fat, 3, FAT_EOC);
        assert_eq!(&fat[4..8], &[3, 0, 0xff, 0xff]);
    }
}
