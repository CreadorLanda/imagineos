//! Minimal PS/2 mouse driver for the ImagineOS display server.
//!
//! The controller's second PS/2 port (the auxiliary device) is polled, not
//! interrupt driven: there is no IRQ plumbing for it here yet. The display
//! server decides when to sample by issuing `INPUT_POLL` syscalls, and the
//! syscall layer drains this driver and pushes pointer events into the global
//! input queue.

use core::arch::asm;

const DATA_PORT: u16 = 0x60;
const STATUS_PORT: u16 = 0x64;

const STATUS_OUTPUT_FULL: u8 = 1 << 0;
const STATUS_INPUT_FULL: u8 = 1 << 1;
const STATUS_AUX_DATA: u8 = 1 << 5;

const CMD_ENABLE_AUX: u8 = 0xa8;
const CMD_WRITE_AUX: u8 = 0xd4;
const CMD_ENABLE_REPORTING: u8 = 0xf4;

/// A single decoded mouse report in raw device units.
#[derive(Clone, Copy)]
pub struct MouseReport {
    pub delta_x: i32,
    pub delta_y: i32,
    pub left: bool,
    pub right: bool,
}

/// Bytes of the packet being assembled, and how many have arrived.
static mut PACKET: [u8; 3] = [0; 3];
static mut PACKET_INDEX: usize = 0;

/// Powers up the auxiliary device and asks it to stream 3-byte reports.
///
/// Failures are silent on purpose: a machine without a PS/2 mouse still boots,
/// it simply produces no pointer events.
pub fn init() {
    unsafe {
        // Drain anything the controller buffered during firmware POST.
        let mut guard = 0;
        while inb(STATUS_PORT) & STATUS_OUTPUT_FULL != 0 && guard < 10_000 {
            let _ = inb(DATA_PORT);
            guard += 1;
        }

        write_command(CMD_ENABLE_AUX);
        // Sample rate 100, resolution 8 counts/mm, scaling 1:1, reporting on.
        write_aux(0xf3);
        write_aux(100);
        write_aux(0xe8);
        write_aux(0x03);
        write_aux(0xe6);
        write_aux(CMD_ENABLE_REPORTING);

        PACKET = [0; 3];
        PACKET_INDEX = 0;
    }
}

/// Lê **todos** os pacotes disponíveis e devolve o movimento acumulado mais o
/// estado final dos botões.
///
/// O PS/2 entrega pacotes a ~100 Hz; o display server consulta a cadência que
/// consegue. Drenar um pacote por chamada deixava o cursor vários pacotes atrás
/// (o "lag" relatado) e, quando bytes do teclado se intercalavam, os pacotes
/// dessincronizavam e o cursor saltava para posições aleatórias.
pub fn poll() -> Option<MouseReport> {
    unsafe {
        let mut accumulated_x = 0i32;
        let mut accumulated_y = 0i32;
        let mut left = false;
        let mut right = false;
        let mut have_report = false;
        // Orçamento alto: drena o controlador, mas não gira infinito.
        let mut budget = 256;
        while inb(STATUS_PORT) & STATUS_OUTPUT_FULL != 0 && budget != 0 {
            budget -= 1;
            let status = inb(STATUS_PORT);
            // Não consuma dados da porta 1: pertencem ao teclado e serão lidos
            // pelo seu dono no próximo `input_poll`. Quebrar aqui (em vez de ler
            // e descartar) preserva o byte para o teclado.
            if status & STATUS_AUX_DATA == 0 {
                break;
            }
            let byte = inb(DATA_PORT);
            PACKET[PACKET_INDEX] = byte;
            PACKET_INDEX += 1;
            if PACKET_INDEX != 3 {
                continue;
            }
            PACKET_INDEX = 0;
            let flags = PACKET[0];
            if flags & 0x08 == 0 {
                // Bit reservado deve estar setado; uma sequência dessincronizada
                // é realinhada descartando o pacote.
                continue;
            }
            let overflow_x = flags & 0x40 != 0;
            let overflow_y = flags & 0x80 != 0;
            // O sinal está nos bits 4/5 do primeiro byte, não no bit 7 do byte
            // baixo. A decodificação de 9 bits já produz o delta com o sinal
            // correto.
            let signed_x = decode_delta(PACKET[1], flags & 0x10 != 0);
            let signed_y = decode_delta(PACKET[2], flags & 0x20 != 0);
            // Convenção PS/2: delta_y positivo = mouse para baixo, mas a
            // origem Y da tela é o topo, então invertemos o sinal para que
            // mover o mouse para baixo mova o cursor para baixo.
            if !overflow_x {
                accumulated_x += signed_x;
            }
            if !overflow_y {
                accumulated_y -= signed_y;
            }
            left = flags & 0x01 != 0;
            right = flags & 0x02 != 0;
            have_report = true;
        }
        if !have_report {
            return None;
        }
        Some(MouseReport {
            delta_x: accumulated_x,
            delta_y: accumulated_y,
            left,
            right,
        })
    }
}

/// Converte byte baixo + bit de sinal do pacote PS/2 em um delta de 9 bits.
fn decode_delta(low: u8, negative: bool) -> i32 {
    if negative {
        i32::from(low) - 256
    } else {
        i32::from(low)
    }
}

/// Writes `value` to the mouse, consuming its 0xfa acknowledge. A missing
/// device simply times out and is ignored.
unsafe fn write_aux(value: u8) {
    write_command(CMD_WRITE_AUX);
    wait_for_input_clear();
    outb(DATA_PORT, value);
    wait_for_ack();
}

unsafe fn write_command(value: u8) {
    wait_for_input_clear();
    outb(STATUS_PORT, value);
}

unsafe fn wait_for_ack() {
    for _ in 0..100_000 {
        if inb(STATUS_PORT) & STATUS_OUTPUT_FULL != 0 {
            let _ = inb(DATA_PORT);
            return;
        }
        asm!("pause", options(nomem, nostack, preserves_flags));
    }
}

unsafe fn wait_for_input_clear() {
    for _ in 0..100_000 {
        if inb(STATUS_PORT) & STATUS_INPUT_FULL == 0 {
            return;
        }
        asm!("pause", options(nomem, nostack, preserves_flags));
    }
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    value
}

unsafe fn outb(port: u16, value: u8) {
    asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
}
