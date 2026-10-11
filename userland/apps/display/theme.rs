//! Tema e fonte bitmap 8x8 do window manager.
//!
//! As cores são RGB de 24 bits; `display::Framebuffer::color` as converte para
//! o formato nativo do hardware. A fonte 8x8 vive na crate `imagineos` e é
//! compartilhada com os clientes de superfície, para que o texto de um painel
//! externo tenha o mesmo aspecto das janelas internas.

pub const BORDER: i32 = 2;
pub const GLYPH_WIDTH: usize = 8;
pub const GLYPH_HEIGHT: usize = 8;
pub const TITLE_HEIGHT: i32 = 18;
pub const TITLE_BUTTON_SIZE: i32 = 12;
pub const CURSOR_SIZE: i32 = 10;

pub const DESKTOP: u32 = 0x17212b;
pub const DESKTOP_ACCENT: u32 = 0x1f6f78;
pub const WINDOW: u32 = 0xe8edf2;
pub const WINDOW_ALT: u32 = 0xd8e0e8;
pub const TITLE_ACTIVE: u32 = 0x176b87;
pub const TITLE_INACTIVE: u32 = 0x465563;
pub const TEXT: u32 = 0x15212b;
pub const TITLE_TEXT: u32 = 0xffffff;
pub const BORDER_COLOR: u32 = 0x0b1118;
pub const BUTTON_CLOSE: u32 = 0xc94c4c;
pub const BUTTON_MINIMIZE: u32 = 0xe0ad4f;
pub const CURSOR_COLOR: u32 = 0xffffff;
pub const CURSOR_OUTLINE: u32 = 0x101820;

/// Devolve um glifo 8x8 (uma linha = 8 bits, bit alto é o pixel à esquerda).
/// Delegado à fonte compartilhada da crate `imagineos`.
pub fn glyph(character: char) -> [u8; GLYPH_HEIGHT] {
    imagineos::font::glyph(character)
}
