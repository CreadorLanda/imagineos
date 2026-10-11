//! Servidor de display e window manager do ImagineOS.
//!
//! É um processo comum de userspace. Ele:
//!
//! 1. mapeia o framebuffer e toma a tela do console do kernel;
//! 2. mantém uma lista fixa de janelas, cada uma com um buffer de pixels e uma
//!    área na tela;
//! 3. encaminha os eventos de entrada para a janela sob o ponteiro;
//! 4. compõe tudo na tela a cada quadro.
//!
//! Não há aceleração nem memória compartilhada entre clientes: as janelas são
//! internas a este processo, criadas pelas "aplicações" definidas em
//! `apps.rs`. O objetivo é exercitar o kernel (framebuffer mapeado em
//! ring 3, eventos, preempção) sem introduzir um protocolo de IPC de vídeo.
//!
//! Uso: `/bin/display` — sai com `q` ou Ctrl+C.

#![no_std]
#![no_main]

mod apps;
mod compositor;
mod theme;
mod window;

use core::panic::PanicInfo;

use compositor::Compositor;
use imagineos::display;

#[no_mangle]
extern "C" fn _start(
    _argc: usize,
    _argv: *const *const u8,
    _envc: usize,
    _envp: *const *const u8,
) -> ! {
    let framebuffer = match display::map_framebuffer() {
        Ok(framebuffer) => framebuffer,
        Err(_) => {
            let _ = imagineos::console::write_all(b"display: nao foi possivel mapear a tela\n");
            imagineos::process::exit(1);
        }
    };
    if display::release_console().is_err() {
        let _ = imagineos::console::write_all(b"display: nao foi possivel tomar a tela\n");
        imagineos::process::exit(1);
    }

    let mut compositor = Compositor::new(framebuffer);
    compositor.spawn_demo_windows();
    compositor.run()
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    // Devolve a tela ao console antes de morrer, senão o sistema fica sem saída
    // visível depois de um erro do servidor.
    let _ = imagineos::console::write_all(b"display: panic\n");
    imagineos::process::exit(101)
}
