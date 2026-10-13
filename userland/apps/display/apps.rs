//! Conteúdo das janelas de demonstração do display server.
//!
//! São "clientes" embutidos no processo do WM para testar composição e texto;
//! um protocolo IPC de janelas ainda não existe.

use crate::window::Window;

pub fn draw_terminal(window: &mut Window) {
    window.clear();
    window.text(10, 12, "ImagineOS display server", window.foreground);
    window.text(10, 32, "Framebuffer em ring 3.", window.foreground);
    window.text(10, 48, "Arraste a barra de titulo.", window.foreground);
    window.text(10, 72, "Teclado + mouse PS/2", window.foreground);
    window.text(10, 104, "Kernel: Rust / no_std", window.foreground);
    window.outline(6, 6, window.width - 12, window.height - 12, 1, window.foreground);
}

pub fn draw_monitor(window: &mut Window) {
    window.clear();
    window.text(8, 8, "WM de pesquisa", window.foreground);
    window.text(8, 24, "2 janelas ativas", window.foreground);
    window.text(8, 40, "Clique na barra para focar", window.foreground);
    window.fill(8, 64, 120, 3, window.foreground);
    window.text(8, 76, "Tecla q encerra", window.foreground);
}
