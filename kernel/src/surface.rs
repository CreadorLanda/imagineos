//! Registro de superfícies de clientes do servidor de display.
//!
//! O protocolo é cliente/servidor, como o X11 simplificado:
//!
//! - o **cliente** (qualquer processo) registra uma superfície com
//!   `SYS_SURFACE_CREATE`; o kernel aloca um buffer de pixels e o mapeia no
//!   espaço do cliente em `SURFACE_REGION_BASE + id * STRIDE`;
//! - o **cliente** escreve o frame no buffer e chama `SYS_SURFACE_FLUSH`;
//! - o **servidor** (dono da tela, quem chamou `SYS_CONSOLE_RELEASE`) recebe
//!   os ids atualizados via `surface_server`/`take_dirty` e copia os buffers
//!   na composição;
//! - o **servidor** entrega eventos dirigidos a uma superfície por PID
//!   (`push_event`), e o cliente os lê com `SYS_SURFACE_POLL`.
//!
//! Não há controle de acesso além do PID: um cliente só enxerga o próprio
//! buffer, e o servidor enxerga todos — o que é aceitável num sistema de
//! desenvolvimento (o servidor é quem já controla a tela).

use core::cell::UnsafeCell;

use imagineos_abi::{
    SurfaceEvent, SurfaceInfo, MAX_SURFACES, SURFACE_EVENT_HIDDEN, SURFACE_MAX_PIXELS,
};

/// Altura máxima de fila de eventos por cliente.
const EVENT_QUEUE: usize = 16;

#[derive(Clone, Copy)]
struct Surface {
    pid: usize,
    width: i32,
    height: i32,
    /// Endereço físico do buffer (visível a todos os espaços via HHDM).
    physical: u64,
    /// Quantidade de pixels válidos por linha no buffer.
    pitch: u32,
    /// Visível na composição.
    shown: bool,
    /// Ordem de empilhamento (maior = à frente), incrementado a cada foco.
    z: u32,
}

impl Surface {
    const EMPTY: Self = Self {
        pid: 0,
        width: 0,
        height: 0,
        physical: 0,
        pitch: 0,
        shown: false,
        z: 0,
    };
}

struct ClientEvents {
    queue: [Option<SurfaceEvent>; EVENT_QUEUE],
    head: usize,
    tail: usize,
    length: usize,
}

impl ClientEvents {
    const EMPTY: Self = Self {
        queue: [None; EVENT_QUEUE],
        head: 0,
        tail: 0,
        length: 0,
    };

    fn push(&mut self, event: SurfaceEvent) -> bool {
        if self.length == EVENT_QUEUE {
            // Fila cheia: descarta o mais antigo. Movimento vira posição
            // recente, que é o que importa.
            self.head = (self.head + 1) % EVENT_QUEUE;
            self.length -= 1;
        }
        self.queue[self.tail] = Some(event);
        self.tail = (self.tail + 1) % EVENT_QUEUE;
        self.length += 1;
        true
    }

    fn pop(&mut self) -> Option<SurfaceEvent> {
        if self.length == 0 {
            return None;
        }
        let event = self.queue[self.head].take();
        self.head = (self.head + 1) % EVENT_QUEUE;
        self.length -= 1;
        event
    }
}

struct Surfaces {
    slots: [Surface; MAX_SURFACES],
    /// Z sequence para que superfícies recém-focadas venham à frente.
    z_counter: u32,
    /// Ids de superfícies com frame novo, consumidos pelo servidor.
    dirty: [u32; MAX_SURFACES],
    dirty_head: usize,
    dirty_tail: usize,
    dirty_length: usize,
    /// PID do servidor de display (o que chamou `SYS_CONSOLE_RELEASE`).
    server_pid: usize,
    /// Fila de eventos por dono de superfície (indexada por slot de processo).
    events: [ClientEvents; imagineos_abi::MAX_PROCESSES],
}

struct SharedSurfaces(UnsafeCell<Surfaces>);
unsafe impl Sync for SharedSurfaces {}

// SAFETY: o acesso é mediado por interrupções desligadas nos handlers de
// syscall (mesma disciplina das filas de entrada e de pipes).
static SURFACES: SharedSurfaces = SharedSurfaces(UnsafeCell::new(Surfaces {
    slots: [Surface::EMPTY; MAX_SURFACES],
    z_counter: 0,
    dirty: [0; MAX_SURFACES],
    dirty_head: 0,
    dirty_tail: 0,
    dirty_length: 0,
    server_pid: 0,
    events: [ClientEvents::EMPTY; imagineos_abi::MAX_PROCESSES],
}));

fn surfaces() -> &'static mut Surfaces {
    unsafe { &mut *SURFACES.0.get() }
}

/// Erros de superfície devolvidos como -errno.
pub const EAGAIN: i64 = -11;
pub const EINVAL: i64 = -22;
pub const ENOSPC: i64 = -28;
pub const ENOENT: i64 = -2;

/// Registra uma superfície para o processo atual, mapeando o buffer no
/// espaço dele. `info.width`/`height` vêm do cliente; `info.id`, `pitch` e
/// `info.address` são preenchidos pelo kernel.
pub fn create(info: &mut SurfaceInfo) -> Result<(), i64> {
    if info.width == 0
        || info.height == 0
        || info.width > imagineos_abi::SURFACE_MAX_WIDTH
        || info.height > imagineos_abi::SURFACE_MAX_HEIGHT
    {
        return Err(EINVAL);
    }
    let surfaces = surfaces();
    let Some(slot) = (0..MAX_SURFACES).find(|&index| surfaces.slots[index].physical == 0) else {
        return Err(ENOSPC);
    };
    let Some(frame) = crate::memory::allocate_frame() else {
        return Err(ENOSPC);
    };
    let pid = crate::process::current_pid();
    // O buffer é zerado via HHDM (o kernel enxerga qualquer frame físico).
    unsafe {
        core::ptr::write_bytes(frame.virtual_address, 0, SURFACE_MAX_PIXELS * 4);
    }
    // Mapeia no espaço do chamador, em fatia fixa por id.
    let client_address =
        imagineos_abi::SURFACE_REGION_BASE + (slot as u64) * imagineos_abi::SURFACE_REGION_STRIDE;
    let mapped = crate::process::with_current_space(|space| {
        space.map_framebuffer_range(
            client_address,
            frame.physical_address,
            SURFACE_MAX_PIXELS * 4,
        )
    });
    if mapped.is_none() {
        return Err(ENOSPC);
    }
    surfaces.slots[slot] = Surface {
        pid,
        width: info.width as i32,
        height: info.height as i32,
        physical: frame.physical_address,
        pitch: info.width,
        shown: false,
        z: surfaces.z_counter,
    };
    surfaces.z_counter = surfaces.z_counter.wrapping_add(1);
    info.id = slot as u32;
    info.pitch = info.width;
    info.address = client_address;
    Ok(())
}

fn find(surface_id: u32) -> Option<usize> {
    let surfaces = surfaces();
    let slot = surface_id as usize;
    if slot >= MAX_SURFACES || surfaces.slots[slot].physical == 0 {
        return None;
    }
    Some(slot)
}

/// Marca a superfície como visível/oculta. Só o dono pode fazê-lo.
pub fn present(surface_id: u32, shown: bool) -> Result<(), i64> {
    let surfaces = surfaces();
    let Some(slot) = find(surface_id) else {
        return Err(ENOENT);
    };
    let pid = crate::process::current_pid();
    if surfaces.slots[slot].pid != pid {
        return Err(EINVAL);
    }
    surfaces.slots[slot].shown = shown;
    if shown {
        surfaces.slots[slot].z = surfaces.z_counter;
        surfaces.z_counter = surfaces.z_counter.wrapping_add(1);
    }
    dirty(surface_id);
    Ok(())
}

/// Publica um frame: coloca o id na fila de sujos para o servidor copiar.
pub fn flush(surface_id: u32) -> Result<(), i64> {
    let surfaces = surfaces();
    let Some(slot) = find(surface_id) else {
        return Err(ENOENT);
    };
    let pid = crate::process::current_pid();
    if surfaces.slots[slot].pid != pid {
        return Err(EINVAL);
    }
    dirty(surface_id);
    Ok(())
}

/// Remove a superfície e desmapeia o buffer no espaço do dono. O buffer
/// físico não é devolvido: o frame allocator é monotônico (sem reclaim).
pub fn destroy(surface_id: u32) -> Result<(), i64> {
    let surfaces = surfaces();
    let Some(slot) = find(surface_id) else {
        return Err(ENOENT);
    };
    let pid = crate::process::current_pid();
    if surfaces.slots[slot].pid != pid {
        return Err(EINVAL);
    }
    surfaces.slots[slot] = Surface::EMPTY;
    Ok(())
}

/// Remove as superfícies de um processo que está terminando.
pub fn release_pid(pid: usize) {
    let surfaces = surfaces();
    for slot in 0..MAX_SURFACES {
        if surfaces.slots[slot].pid == pid {
            surfaces.slots[slot] = Surface::EMPTY;
        }
    }
    surfaces.events[pid % imagineos_abi::MAX_PROCESSES] = ClientEvents::EMPTY;
}

fn dirty(surface_id: u32) {
    let surfaces = surfaces();
    if surfaces.dirty_length == MAX_SURFACES {
        // Já cheia: uma chamada extra de flush é inofensiva.
        return;
    }
    surfaces.dirty[surfaces.dirty_tail] = surface_id;
    surfaces.dirty_tail = (surfaces.dirty_tail + 1) % MAX_SURFACES;
    surfaces.dirty_length += 1;
}

/// Fila de sujos do servidor: retira o próximo id com frame novo.
pub fn take_dirty() -> Option<u32> {
    let surfaces = surfaces();
    if surfaces.dirty_length == 0 {
        return None;
    }
    let id = surfaces.dirty[surfaces.dirty_head];
    surfaces.dirty_head = (surfaces.dirty_head + 1) % MAX_SURFACES;
    surfaces.dirty_length -= 1;
    Some(id)
}

/// Snapshot de uma superfície para o compositor desenhar (formato da ABI,
/// já com o buffer mapeado no espaço do servidor).
pub type Snapshot = imagineos_abi::SurfaceSnapshot;

/// Lista as superfícies visíveis e mapeia os buffers no processo atual (o
/// servidor de display). Devolve a quantidade preenchida.
pub fn snapshot_mapped(output: &mut [Snapshot]) -> usize {
    let surfaces = surfaces();
    let mut count = 0;
    for (slot, surface) in surfaces.slots.iter().enumerate() {
        if surface.physical == 0 || !surface.shown || count >= output.len() {
            continue;
        }
        // Mapeia o buffer do cliente no espaço do servidor, numa fatia fixa
        // acima do framebuffer (nunca colide: base 0x5000_0000_0000).
        const SERVER_BASE: u64 = 0x0000_5000_0000_0000;
        const SERVER_STRIDE: u64 = 0x10_0000;
        let virtual_address =
            SERVER_BASE + (slot as u64) * SERVER_STRIDE;
        let mapped = crate::process::with_current_space(|space| {
            space.map_framebuffer_range(
                virtual_address,
                surface.physical,
                SURFACE_MAX_PIXELS * 4,
            )
        });
        if mapped.is_none() {
            continue;
        }
        output[count] = Snapshot {
            id: slot as u32,
            width: surface.width as u32,
            height: surface.height as u32,
            pitch: surface.pitch,
            address: virtual_address,
        };
        count += 1;
    }
    count
}

/// Registra o PID do servidor de display (o que assumiu a tela).
pub fn set_server() {
    surfaces().server_pid = crate::process::current_pid();
}

/// Ponto de entrada dos eventos: o servidor entrega, os clientes leem.
///
/// O roteamento é por dono: o compositor traduz a posição global do mouse na
/// superfície sob o cursor e chama `push_event_for(pid, ...)`.
pub fn push_event_for(pid: usize, event: SurfaceEvent) {
    let surfaces = surfaces();
    let _ = surfaces.events[pid % imagineas_max()].push(event);
}

fn imagineas_max() -> usize {
    imagineos_abi::MAX_PROCESSES
}

/// Entrega um evento ao dono da superfície indicada.
pub fn push_event_to_surface(surface_id: u32, event: SurfaceEvent) -> bool {
    let surfaces = surfaces();
    let Some(slot) = find(surface_id) else {
        return false;
    };
    let pid = surfaces.slots[slot].pid;
    let index = pid % imagineas_max();
    surfaces.events[index].push(event)
}

/// Notifica o dono que a superfície foi escondida pelo WM.
pub fn notify_hidden(surface_id: u32) {
    let _ = push_event_to_surface(
        surface_id,
        SurfaceEvent {
            surface: surface_id,
            kind: SURFACE_EVENT_HIDDEN,
            ..SurfaceEvent::default()
        },
    );
}

/// Retira o próximo evento dirigido ao processo atual.
pub fn pop_event() -> Option<SurfaceEvent> {
    let surfaces = surfaces();
    let pid = crate::process::current_pid();
    surfaces.events[pid % imagineas_max()].pop()
}
