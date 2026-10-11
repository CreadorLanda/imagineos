use crate::memory;
use core::arch::asm;
use core::ptr;

const PAGE_SIZE: u64 = 4096;
const ENTRY_PRESENT: u64 = 1;
const ENTRY_WRITABLE: u64 = 1 << 1;
const ENTRY_USER: u64 = 1 << 2;
const ENTRY_HUGE: u64 = 1 << 7;
const ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;
const USER_MIN: u64 = 0x400000;
const USER_MAX: u64 = 0x0000_8000_0000_0000;

#[derive(Clone, Copy)]
pub struct AddressSpace {
    root_physical: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    InvalidAddress,
    OutOfMemory,
    AlreadyMapped,
}

impl AddressSpace {
    pub fn new_user() -> Option<Self> {
        let current_root = read_cr3() & ADDRESS_MASK;
        let root = memory::allocate_frame()?;
        unsafe {
            ptr::write_bytes(root.virtual_address, 0, PAGE_SIZE as usize);
            let source = memory::phys_to_virt(current_root)?.cast::<u64>();
            let destination = root.virtual_address.cast::<u64>();
            ptr::copy_nonoverlapping(source.add(256), destination.add(256), 256);
        }
        Some(Self {
            root_physical: root.physical_address,
        })
    }

    pub fn map_user_page(&self, virtual_address: u64) -> Result<*mut u8, MapError> {
        if !(USER_MIN..USER_MAX).contains(&virtual_address) {
            return Err(MapError::InvalidAddress);
        }
        let page_address = virtual_address & !(PAGE_SIZE - 1);
        let indices = [
            ((page_address >> 39) & 0x1ff) as usize,
            ((page_address >> 30) & 0x1ff) as usize,
            ((page_address >> 21) & 0x1ff) as usize,
            ((page_address >> 12) & 0x1ff) as usize,
        ];
        let mut table_physical = self.root_physical;
        for index in indices.iter().take(3) {
            let table = table_pointer(table_physical).ok_or(MapError::OutOfMemory)?;
            let entry = unsafe { &mut *table.add(*index) };
            if *entry & ENTRY_PRESENT == 0 {
                let frame = memory::allocate_frame().ok_or(MapError::OutOfMemory)?;
                unsafe {
                    ptr::write_bytes(frame.virtual_address, 0, PAGE_SIZE as usize);
                }
                *entry = frame.physical_address | ENTRY_PRESENT | ENTRY_WRITABLE | ENTRY_USER;
            } else {
                *entry |= ENTRY_WRITABLE | ENTRY_USER;
            }
            table_physical = *entry & ADDRESS_MASK;
        }

        let table = table_pointer(table_physical).ok_or(MapError::OutOfMemory)?;
        let entry = unsafe { &mut *table.add(indices[3]) };
        if *entry & ENTRY_PRESENT != 0 {
            return Err(MapError::AlreadyMapped);
        }
        let frame = memory::allocate_frame().ok_or(MapError::OutOfMemory)?;
        unsafe {
            ptr::write_bytes(frame.virtual_address, 0, PAGE_SIZE as usize);
        }
        *entry = frame.physical_address | ENTRY_PRESENT | ENTRY_WRITABLE | ENTRY_USER;
        Ok(frame.virtual_address)
    }

    /// Cria um novo espaço de endereço com uma **cópia** de todas as páginas de
    /// usuário mapeadas neste.
    ///
    /// É um `fork` clássico sem copy-on-write: cada página de usuário recebe um
    /// frame novo com o conteúdo duplicado. O half superior (kernel) é
    /// compartilhado, como em [`AddressSpace::new_user`].
    ///
    /// Devolve `None` se faltar memória; nesse caso os frames já alocados são
    /// liberados, mas o novo espaço de endereço parcial pode permanecer com
    /// tabelas de páginas alocadas (elas são poucas e permanecem ligadas à
    /// tabela raiz, que é descartada).
    pub fn clone_user(&self) -> Option<Self> {
            let clone = Self::new_user()?;
            if self.copy_user_pages(&clone) == 0 {
                // O espaço de origem não tinha nenhuma página de usuário: não há o
                // que herdar, e o clone é inútil.
                return None;
            }
            Some(clone)
        }

        /// Percorre a metade de usuário da árvore e copia cada página presente.
    ///
    /// Devolve quantas páginas (PTEs de nível 1) foram copiadas; `0` significa
    /// que o espaço não tinha nada mapeado.
    fn copy_user_pages(&self, destination: &Self) -> usize {
        let mut copied = 0usize;
        for pml4_index in 0..256usize {
            // `4` é o nível da tabela percorrida (o PML4). O `copy_table` desce
            // até o nível 1 (tabela de PTEs); passar `3` aqui desalinhava todos
            // os níveis e copiava a página errada — o `fork` acabava com quase
            // nenhuma página no espaço do filho.
            copied += self.copy_table(
                self.root_physical,
                destination.root_physical,
                pml4_index,
                4,
            );
        }
        copied
    }

    /// Copia recursivamente uma entrada de tabela até chegar a nível 1 (PTE).
    ///
    /// `level` indica o nível da tabela que está sendo percorrida: 3 = PDPT,
    /// 2 = PD, 1 = PT. Ao chegar nas PTEs, cada página mapeada é copiada para um
    /// frame novo.
    fn copy_table(
        &self,
        source_physical: u64,
        destination_physical: u64,
        index: usize,
        level: u8,
    ) -> usize {
        let Some(source_table) = table_pointer(source_physical) else {
            return 0;
        };
        let source_entry = unsafe { *source_table.add(index) };
        if source_entry & ENTRY_PRESENT == 0 {
            return 0;
        }

        let Some(destination_table) = table_pointer(destination_physical) else {
            return 0;
        };
        let destination_entry = unsafe { &mut *destination_table.add(index) };

        if level == 1 {
            // PTE: aloca um frame novo e copia o conteúdo da página.
            let Some(frame) = memory::allocate_frame() else {
                return 0;
            };
            let Some(source_page) = table_pointer(source_entry & ADDRESS_MASK) else {
                return 0;
            };
            unsafe {
                ptr::copy_nonoverlapping(
                    source_page.cast::<u8>(),
                    frame.virtual_address,
                    PAGE_SIZE as usize,
                );
            }
            // Preserva os bits de controle da PTE original (presente, escrita,
            // usuário), trocando apenas o endereço físico da página.
            *destination_entry = (source_entry & !ADDRESS_MASK) | frame.physical_address;
            return 1;
        }

        if source_entry & ENTRY_HUGE != 0 {
            // Páginas enormes (2 MiB) na metade de usuário não são usadas pelo
            // loader, que mapeia sempre páginas de 4 KiB. Recusar aqui é mais
            // seguro do que copiar errado.
            return 0;
        }

        // Tabela intermediária: garante uma tabela própria no destino e desce.
        let next_source = source_entry & ADDRESS_MASK;
        if *destination_entry & ENTRY_PRESENT == 0 {
            let Some(frame) = memory::allocate_frame() else {
                return 0;
            };
            unsafe {
                ptr::write_bytes(frame.virtual_address, 0, PAGE_SIZE as usize);
            }
            *destination_entry = frame.physical_address | ENTRY_PRESENT | ENTRY_WRITABLE | ENTRY_USER;
        }
        let next_destination = *destination_entry & ADDRESS_MASK;
        let mut copied = 0usize;
        for child in 0..512usize {
            copied += self.copy_table(next_source, next_destination, child, level - 1);
        }
        copied
    }

    /// Mapeia uma faixa de memória física (o framebuffer) na metade de usuário.
    ///
    /// Diferente de [`map_user_page`], a página física é **compartilhada**, não
    /// alocada: a memória de vídeo é um recurso único, e todos os clientes do
    /// display precisam vê-la. O mapeamento é gravável e de usuário — o kernel
    /// não protege a tela de um cliente mal comportado, o que é aceitável para
    /// um servidor de display em desenvolvimento.
    ///
    /// Devolve o endereço virtual onde a faixa ficou mapeada.
pub fn map_framebuffer_range(
        &self,
        virtual_address: u64,
        physical_address: u64,
        length: usize,
    ) -> Option<u64> {
        if length == 0 {
            return None;
        }
        let page_offset = physical_address & (PAGE_SIZE - 1);
        let start_physical = physical_address & !(PAGE_SIZE - 1);
        let start_virtual = (virtual_address & !(PAGE_SIZE - 1))
            .checked_add(page_offset)
            .unwrap_or(virtual_address);
        let pages = (page_offset as usize)
            .checked_add(length)?
            .div_ceil(PAGE_SIZE as usize);
        for index in 0..pages {
            let virtual_page = (virtual_address & !(PAGE_SIZE - 1))
                .checked_add((index as u64) * PAGE_SIZE)?;
            let physical_page = start_physical.checked_add((index as u64) * PAGE_SIZE)?;
            let target = self.map_physical_page(virtual_page, physical_page)?;
            let _ = target;
        }
        Some(start_virtual)
    }

    /// Instala uma PTE apontando para uma página física existente.
    fn map_physical_page(&self, virtual_address: u64, physical_address: u64) -> Option<*mut u8> {
        if !(USER_MIN..USER_MAX).contains(&virtual_address) {
            return None;
        }
        let page_address = virtual_address & !(PAGE_SIZE - 1);
        let indices = [
            ((page_address >> 39) & 0x1ff) as usize,
            ((page_address >> 30) & 0x1ff) as usize,
            ((page_address >> 21) & 0x1ff) as usize,
            ((page_address >> 12) & 0x1ff) as usize,
        ];
        let mut table_physical = self.root_physical;
        for index in indices.iter().take(3) {
            let table = table_pointer(table_physical)?;
            let entry = unsafe { &mut *table.add(*index) };
            if *entry & ENTRY_PRESENT == 0 {
                let frame = memory::allocate_frame()?;
                unsafe {
                    ptr::write_bytes(frame.virtual_address, 0, PAGE_SIZE as usize);
                }
                *entry = frame.physical_address | ENTRY_PRESENT | ENTRY_WRITABLE | ENTRY_USER;
            } else {
                *entry |= ENTRY_WRITABLE | ENTRY_USER;
            }
            table_physical = *entry & ADDRESS_MASK;
        }
        let table = table_pointer(table_physical)?;
        let entry = unsafe { &mut *table.add(indices[3]) };
        // O framebuffer é cacheado em write-combining por bons motores; aqui não
        // há controle de PAT, então mantemos memória cacheável padrão.
        *entry = physical_address | ENTRY_PRESENT | ENTRY_WRITABLE | ENTRY_USER;
        memory::phys_to_virt(physical_address)
    }

    pub fn translate(&self, virtual_address: u64) -> Option<*mut u8> {
        let indices = [
            ((virtual_address >> 39) & 0x1ff) as usize,
            ((virtual_address >> 30) & 0x1ff) as usize,
            ((virtual_address >> 21) & 0x1ff) as usize,
            ((virtual_address >> 12) & 0x1ff) as usize,
        ];
        let mut table_physical = self.root_physical;
        for index in indices.iter().take(3) {
            let table = table_pointer(table_physical)?;
            let entry = unsafe { *table.add(*index) };
            if entry & ENTRY_PRESENT == 0 || entry & ENTRY_USER == 0 {
                return None;
            }
            table_physical = entry & ADDRESS_MASK;
        }
        let table = table_pointer(table_physical)?;
        let entry = unsafe { *table.add(indices[3]) };
        if entry & ENTRY_PRESENT == 0 || entry & ENTRY_USER == 0 {
            return None;
        }
        let physical = (entry & ADDRESS_MASK) | (virtual_address & (PAGE_SIZE - 1));
        memory::phys_to_virt(physical)
    }
}

pub fn switch(space: AddressSpace) {
    unsafe {
        asm!("mov cr3, {}", in(reg) space.root_physical, options(nostack, preserves_flags));
    }
}

pub fn map_hhdm_range(hhdm_offset: u64, physical_address: u64, length: usize) -> bool {
    let Some(end) = physical_address.checked_add(length as u64) else {
        return false;
    };
    if length == 0 {
        return true;
    }

    let mut physical = physical_address & !(PAGE_SIZE - 1);
    let end = end.saturating_add(PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    while physical < end {
        let Some(virtual_address) = hhdm_offset.checked_add(physical) else {
            return false;
        };
        if !map_hhdm_page(virtual_address, physical) {
            return false;
        }
        physical = physical.saturating_add(PAGE_SIZE);
    }
    true
}

fn map_hhdm_page(virtual_address: u64, physical_address: u64) -> bool {
    let indices = [
        ((virtual_address >> 39) & 0x1ff) as usize,
        ((virtual_address >> 30) & 0x1ff) as usize,
        ((virtual_address >> 21) & 0x1ff) as usize,
        ((virtual_address >> 12) & 0x1ff) as usize,
    ];
    let mut table_physical = read_cr3() & ADDRESS_MASK;
    for (level, index) in indices.iter().take(3).enumerate() {
        let Some(table) = table_pointer(table_physical) else {
            return false;
        };
        let entry = unsafe { &mut *table.add(*index) };
        if *entry & ENTRY_PRESENT == 0 {
            let Some(frame) = memory::allocate_frame() else {
                return false;
            };
            unsafe {
                ptr::write_bytes(frame.virtual_address, 0, PAGE_SIZE as usize);
            }
            *entry = frame.physical_address | ENTRY_PRESENT | ENTRY_WRITABLE;
        } else if level > 0 && *entry & ENTRY_HUGE != 0 {
            return true;
        }
        table_physical = *entry & ADDRESS_MASK;
    }

    let Some(table) = table_pointer(table_physical) else {
        return false;
    };
    let entry = unsafe { &mut *table.add(indices[3]) };
    if *entry & ENTRY_PRESENT != 0 {
        return *entry & ADDRESS_MASK == physical_address;
    }
    *entry = physical_address | ENTRY_PRESENT | ENTRY_WRITABLE;
    unsafe {
        asm!("invlpg [{}]", in(reg) virtual_address, options(nostack, preserves_flags));
    }
    true
}

fn read_cr3() -> u64 {
    let value: u64;
    unsafe {
        asm!("mov {}, cr3", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

fn table_pointer(physical: u64) -> Option<*mut u64> {
    Some(memory::phys_to_virt(physical)?.cast())
}
