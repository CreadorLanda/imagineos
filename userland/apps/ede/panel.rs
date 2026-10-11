//! ede-panel — primeiro cliente externo do protocolo de superfícies.
//!
//! Inspirado no Equinox Desktop Environment: um painel fino no topo da tela
//! (a superfície 0 é posicionada ali pelo compositor), desenhado por um
//! processo separado do window manager. Mostra o nome do sistema e os ticks
//! desde o boot, e reagrupa a cada segundo — um ciclo completo de cliente:
//! criar superfície, desenhar, apresentar, esperar eventos, redesenhar.
//!
//! Uso: `/bin/ede-panel` — roda até o WM sair (a superfície é liberada pelo
//! kernel quando o processo termina).

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use imagineos::{font, process, surface, syscall};

const PANEL_WIDTH: usize = 320;
const PANEL_HEIGHT: usize = 22;

const COLOR_BAR: u32 = 0x0b1118;
const COLOR_ACCENT: u32 = 0x1f6f78;
const COLOR_TEXT: u32 = 0xe8edf2;

#[unsafe(no_mangle)]
extern "C" fn _start(
    _argc: usize,
    _argv: *const *const u8,
    _envc: usize,
    _envp: *const *const u8,
) -> ! {
    let panel = match surface::Surface::create(PANEL_WIDTH, PANEL_HEIGHT) {
        Ok(panel) => panel,
        Err(_) => {
            let _ = imagineos::console::write_all(b"ede-panel: sem superficie\n");
            process::exit(1);
        }
    };

    if panel.present().is_err() {
        let _ = imagineos::console::write_all(b"ede-panel: sem compositor\n");
        process::exit(1);
    }

    let mut tick: u32 = 0;
    loop {
        draw(&panel, tick);
        let _ = panel.flush();
        // Dorme ~1 s entre redesenhos: cede a CPU ao invés de girar.
        for _ in 0..25 {
            if process::yield_now().is_err() {
                // O WM saiu ou o sistema desligou; encerra de forma limpa.
                let _ = surface::destroy(&panel);
                process::exit(0);
            }
        }
        tick = tick.wrapping_add(1);
        // Consome eventos (nenhum é tratado pelo painel ainda, mas a fila não
        // pode crescer indefinidamente).
        while let Ok(Some(_)) = surface::poll_event() {}
    }
}

fn draw(panel: &surface::Surface, tick: u32) {
    panel.clear(COLOR_BAR);
    panel.fill(0, PANEL_HEIGHT as i32 - 2, PANEL_WIDTH as i32, 2, COLOR_ACCENT);
    panel.text(8, 7, "EDE panel", COLOR_TEXT);
    panel.text(120, 7, "ImagineOS", COLOR_ACCENT);
    // Relógio: ticks de 1 s desde o início do painel.
    let mut buffer = [0u8; 16];
    let seconds = tick;
    let mut length = 0;
    let mut value = seconds;
    if value == 0 {
        buffer[0] = b'0';
        length = 1;
    }
    while value > 0 {
        buffer[length] = b'0' + (value % 10) as u8;
        length += 1;
        value /= 10;
    }
    for index in 0..length / 2 {
        buffer.swap(index, length - 1 - index);
    }
    buffer[length] = b's';
    // str de buffer: bytes ASCII garantidos acima.
    let text = core::str::from_utf8(&buffer[..length + 1]).unwrap_or("");
    panel.text(PANEL_WIDTH as i32 - 8 - (length as i32 + 1) * 9, 7, text, COLOR_TEXT);
    // Separação entre as seções.
    panel.fill(112, 5, 1, PANEL_HEIGHT as i32 - 10, COLOR_ACCENT);
    let _ = font::GLYPH_WIDTH;
    let _ = syscall::abi_version();
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process::exit(101)
}
