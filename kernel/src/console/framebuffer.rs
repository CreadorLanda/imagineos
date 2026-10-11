#![allow(dead_code)]
use core::cell::UnsafeCell;
use core::ptr;
use limine::framebuffer::Framebuffer;

/* Console structure */
struct Console {
    address: *mut u8,
    width: usize,
    height: usize,
    pitch: usize,
    red_shift: u8,
    green_shift: u8,
    blue_shift: u8,
    cursor_x: usize,
    cursor_y: usize,
    cursor_visible: bool,
    font: *const u8,
    font_size: usize,
    font_width: usize,
    font_height: usize,
    glyph_count: usize,
    glyph_bytes: usize,
    glyph_offset: usize,
    unicode_offset: usize,
    has_unicode_table: bool,
    ansi_state: u8,
    ansi_value: u16,
    ansi_has_value: bool,
    foreground: u32,
    /// Quando falso, o console do kernel não desenha: a tela pertence ao
    /// servidor de display de userspace.
    active: bool,
}

impl Console {
    const fn empty() -> Self {
        Self {
            address: ptr::null_mut(),
            width: 0,
            height: 0,
            pitch: 0,
            red_shift: 0,
            green_shift: 0,
            blue_shift: 0,
            cursor_x: 0,
            cursor_y: 0,
            cursor_visible: false,
            font: ptr::null(),
            font_size: 0,
            font_width: 6,
            font_height: 8,
            glyph_count: 0,
            glyph_bytes: 0,
            glyph_offset: 0,
            unicode_offset: 0,
            has_unicode_table: false,
            ansi_state: 0,
            ansi_value: 0,
            ansi_has_value: false,
            foreground: 0xdce8e8,
            active: true,
        }
    }
}

struct SharedConsole(UnsafeCell<Console>);
unsafe impl Sync for SharedConsole {}
static CONSOLE: SharedConsole = SharedConsole(UnsafeCell::new(Console::empty()));

/// Liga o console do kernel na impressão (usado depois que o servidor de
/// display assume a tela).
#[allow(dead_code)]
pub(crate) fn set_active(active: bool) {
    let console = unsafe { &mut *CONSOLE.0.get() };
    console.active = active;
}

pub fn init(framebuffer: Option<Framebuffer<'_>>) {
    let Some(framebuffer) = framebuffer else {
        return;
    };
    if framebuffer.bpp() != 32 || framebuffer.width() == 0 || framebuffer.height() == 0 {
        return;
    }
    let console = unsafe { &mut *CONSOLE.0.get() };
    console.address = framebuffer.addr();
    console.width = framebuffer.width() as usize;
    console.height = framebuffer.height() as usize;
    console.pitch = framebuffer.pitch() as usize;
    console.red_shift = framebuffer.red_mask_shift();
    console.green_shift = framebuffer.green_mask_shift();
    console.blue_shift = framebuffer.blue_mask_shift();
    clear();
}

pub fn load_font(bytes: &[u8]) -> bool {
    let console = unsafe { &mut *CONSOLE.0.get() };
    if console.address.is_null() {
        return false;
    }
    if bytes.len() >= 4 && bytes[0] == 0x36 && bytes[1] == 0x04 {
        let glyph_count: usize = if bytes[2] & 1 != 0 { 512 } else { 256 };
        let glyph_bytes = bytes[3] as usize;
        let total_bytes = glyph_count.checked_mul(glyph_bytes);
        if glyph_bytes > 0 && total_bytes.is_some_and(|size| size <= bytes.len() - 4) {
            console.font = bytes.as_ptr();
            console.font_size = bytes.len();
            console.font_width = 8;
            console.font_height = glyph_bytes;
            console.glyph_count = glyph_count;
            console.glyph_bytes = glyph_bytes;
            console.glyph_offset = 4;
            console.unicode_offset = 4 + glyph_count * glyph_bytes;
            console.has_unicode_table = bytes[2] & 2 != 0;
            return true;
        }
    }
    if bytes.len() >= 32 && read_u32(bytes, 0) == Some(0x864ab572) {
        let header_size = read_u32(bytes, 8).unwrap_or(0) as usize;
        let glyph_count = read_u32(bytes, 16).unwrap_or(0) as usize;
        let glyph_bytes = read_u32(bytes, 20).unwrap_or(0) as usize;
        let height = read_u32(bytes, 24).unwrap_or(0) as usize;
        let width = read_u32(bytes, 28).unwrap_or(0) as usize;
        let glyph_data_size = glyph_count.checked_mul(glyph_bytes);
        if header_size >= 32
            && header_size <= bytes.len()
            && glyph_count > 0
            && glyph_bytes > 0
            && width > 0
            && height > 0
            && glyph_data_size.is_some_and(|size| {
                header_size
                    .checked_add(size)
                    .is_some_and(|end| end <= bytes.len())
            })
        {
            console.font = bytes.as_ptr();
            console.font_size = bytes.len();
            console.font_width = width;
            console.font_height = height;
            console.glyph_count = glyph_count;
            console.glyph_bytes = glyph_bytes;
            console.glyph_offset = header_size;
            console.unicode_offset = header_size + glyph_data_size.unwrap_or(0);
            console.has_unicode_table = read_u32(bytes, 12).unwrap_or(0) & 1 != 0;
            return true;
        }
    }
    false
}

pub fn write_str(text: &str) {
    for character in text.chars() {
        write_char(character);
    }
}

#[allow(dead_code)]
pub fn write_utf8(bytes: &[u8]) {
    let mut remaining = bytes;
    while !remaining.is_empty() {
        match core::str::from_utf8(remaining) {
            Ok(text) => {
                write_str(text);
                return;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                if valid != 0 {
                    write_str(core::str::from_utf8(&remaining[..valid]).unwrap_or(""));
                }
                write_char('\u{fffd}');
                let invalid = error.error_len().unwrap_or(remaining.len() - valid);
                remaining = &remaining[(valid + invalid).min(remaining.len())..];
            }
        }
    }
}

pub fn write_ansi(bytes: &[u8]) {
    let mut plain = [0u8; 4096];
    let mut plain_length = 0usize;
    for &byte in bytes {
        let flush_plain = {
            let console = unsafe { &mut *CONSOLE.0.get() };
            match console.ansi_state {
                0 if byte == 0x1b => {
                    console.ansi_state = 1;
                    true
                }
                0 => {
                    plain[plain_length] = byte;
                    plain_length += 1;
                    false
                }
                1 => {
                    console.ansi_state = if byte == b'[' { 2 } else { 0 };
                    console.ansi_value = 0;
                    console.ansi_has_value = false;
                    false
                }
                2 if byte.is_ascii_digit() => {
                    console.ansi_value = console
                        .ansi_value
                        .saturating_mul(10)
                        .saturating_add((byte - b'0') as u16);
                    console.ansi_has_value = true;
                    false
                }
                2 if byte == b';' => {
                    if console.ansi_value == 0 {
                        console.foreground = 0xdce8e8;
                    } else {
                        set_ansi_foreground(console, console.ansi_value);
                    }
                    console.ansi_value = 0;
                    console.ansi_has_value = false;
                    false
                }
                2 if byte == b'm' => {
                    if console.ansi_value == 0 || !console.ansi_has_value {
                        console.foreground = 0xdce8e8;
                    } else {
                        set_ansi_foreground(console, console.ansi_value);
                    }
                    console.ansi_state = 0;
                    false
                }
                2 => {
                    console.ansi_state = 0;
                    false
                }
                _ => {
                    console.ansi_state = 0;
                    false
                }
            }
        };
        if flush_plain && plain_length != 0 {
            write_utf8(&plain[..plain_length]);
            plain_length = 0;
        }
    }
    if plain_length != 0 {
        write_utf8(&plain[..plain_length]);
    }
}

fn set_ansi_foreground(console: &mut Console, color: u16) {
    console.foreground = match color {
        30 => 0x101820,
        31 => 0xff6b6b,
        32 => 0x79d279,
        33 => 0xf2c879,
        34 => 0x78a9ff,
        35 => 0xd79bff,
        36 => 0x72d6d6,
        37 => 0xdce8e8,
        90 => 0x74808a,
        91 => 0xff8b8b,
        92 => 0x9be69b,
        93 => 0xffdc8a,
        94 => 0x91b9ff,
        95 => 0xe2afff,
        96 => 0x8ce6e6,
        97 => 0xffffff,
        _ => console.foreground,
    };
}

pub fn write_char(character: char) {
    let console = unsafe { &mut *CONSOLE.0.get() };
    if console.address.is_null() {
        return;
    }
    draw_cursor(console, false);
    match character {
        '\n' => {
            console.cursor_x = 0;
            console.cursor_y += console.font_height;
        }
        '\r' => console.cursor_x = 0,
        '\u{8}' => {
            let previous_x = console.cursor_x;
            console.cursor_x = console.cursor_x.saturating_sub(console.font_width);
            if console.cursor_x != previous_x {
                erase_cell(console, console.cursor_x, console.cursor_y);
            }
        }
        '\t' => {
            let tab_width = console.font_width * 4;
            console.cursor_x = (console.cursor_x / tab_width + 1) * tab_width;
        }
        _ => draw_glyph(console, character),
    }
    if console.cursor_x + console.font_width > console.width {
        console.cursor_x = 0;
        console.cursor_y += console.font_height;
    }
    if console.cursor_y + console.font_height > console.height {
        scroll_one_line(console);
    }
}

/// Descrição do framebuffer para um cliente de display em userspace.
///
/// O servidor de display precisa saber onde desenhar (endereço físico mapeado),
/// o formato e as dimensões. O deslocamento dos canais já vem calculado para
/// que o userspace não precise repetir a aritmética das máscaras do Limine.
#[derive(Clone, Copy)]
pub struct Description {
    pub physical_address: u64,
    pub width: usize,
    pub height: usize,
    pub pitch: usize,
    pub red_shift: u8,
    pub green_shift: u8,
    pub blue_shift: u8,
}

/// Devolve a descrição do framebuffer, ou `None` se não houver um ativo.
pub fn description() -> Option<Description> {
    let console = unsafe { &*CONSOLE.0.get() };
    if console.address.is_null() || console.width == 0 || console.height == 0 {
        return None;
    }
    // O `Console` guarda o ponteiro já resolvido no HHDM. Para o userspace o que
    // importa é o endereço **físico**, que é o que aparece na memória de vídeo.
    let virtual_address = console.address as u64;
    let physical = virtual_address.checked_sub(hhdm_offset())?;
    Some(Description {
        physical_address: physical,
        width: console.width,
        height: console.height,
        pitch: console.pitch,
        red_shift: console.red_shift,
        green_shift: console.green_shift,
        blue_shift: console.blue_shift,
    })
}

/// Guarda o deslocamento do HHDM para converter ponteiros virtuais em físicos.
static mut HHDM_OFFSET: u64 = 0;

fn hhdm_offset() -> u64 {
    unsafe { HHDM_OFFSET }
}

/// Registra o deslocamento do HHDM, necessário para [`description`].
pub fn set_hhdm_offset(offset: u64) {
    unsafe {
        HHDM_OFFSET = offset;
    }
}

/// Limpa a tela com a cor de fundo do console do kernel.
pub(crate) fn clear() {
    let console = unsafe { &mut *CONSOLE.0.get() };
    if console.address.is_null() {
        return;
    }
    for y in 0..console.height {
        for x in 0..console.width {
            pixel(console, x, y, 0x101820);
        }
    }
    console.cursor_x = 0;
    console.cursor_y = 0;
    console.cursor_visible = false;
}

#[allow(dead_code)]
pub(crate) fn set_cursor_visible(visible: bool) {
    let console = unsafe { &mut *CONSOLE.0.get() };
    draw_cursor(console, visible);
}

fn draw_cursor(console: &mut Console, visible: bool) {
    if console.address.is_null() || console.cursor_visible == visible {
        return;
    }
    let row = console.cursor_y + console.font_height.saturating_sub(2);
    let color = if visible { 0xdce8e8 } else { 0x101820 };
    for column in 1..console.font_width.saturating_sub(1) {
        pixel(console, console.cursor_x + column, row, color);
    }
    console.cursor_visible = visible;
}

fn erase_cell(console: &Console, x: usize, y: usize) {
    let width = console.font_width.min(console.width.saturating_sub(x));
    let height = console.font_height.min(console.height.saturating_sub(y));
    for row in y..y + height {
        for column in x..x + width {
            pixel(console, column, row, 0x101820);
        }
    }
}

fn scroll_one_line(console: &mut Console) {
    let rows = console.font_height.min(console.height);
    if rows == 0 {
        return;
    }
    let remaining_rows = console.height - rows;
    for y in 0..remaining_rows {
        for x in 0..console.width {
            let source = unsafe {
                ptr::read_volatile(
                    console
                        .address
                        .add((y + rows) * console.pitch + x * 4)
                        .cast::<u32>(),
                )
            };
            unsafe {
                ptr::write_volatile(
                    console.address.add(y * console.pitch + x * 4).cast::<u32>(),
                    source,
                );
            }
        }
    }
    for y in remaining_rows..console.height {
        for x in 0..console.width {
            pixel(console, x, y, 0x101820);
        }
    }
    console.cursor_y = console.cursor_y.saturating_sub(rows);
    console.cursor_visible = false;
}

fn draw_glyph(console: &mut Console, character: char) {
    if console.font.is_null() {
        return;
    }
    let font = unsafe { core::slice::from_raw_parts(console.font, console.font_size) };
    let glyph = glyph_index(console, font, character).or_else(|| glyph_index(console, font, '?'));
    let Some(glyph) = glyph else {
        return;
    };
    if glyph >= console.glyph_count || console.font_width > 32 || console.font_height > 64 {
        return;
    }
    erase_cell(console, console.cursor_x, console.cursor_y);
    let start = console.glyph_offset + glyph * console.glyph_bytes;
    let row_bytes = (console.font_width + 7) / 8;
    for row in 0..console.font_height {
        for column in 0..console.font_width {
            let byte_index = start + row * row_bytes + column / 8;
            if byte_index < font.len() && font[byte_index] & (0x80 >> (column & 7)) != 0 {
                pixel(
                    console,
                    console.cursor_x + column,
                    console.cursor_y + row,
                    console.foreground,
                );
            }
        }
    }
    console.cursor_x += console.font_width;
}

fn glyph_index(console: &Console, font: &[u8], character: char) -> Option<usize> {
    if character.is_ascii() && (character as usize) < console.glyph_count {
        return Some(character as usize);
    }
    if !console.has_unicode_table || console.unicode_offset >= font.len() {
        return None;
    }
    if font.starts_with(&[0x36, 0x04]) {
        let mut offset = console.unicode_offset;
        for glyph in 0..console.glyph_count {
            while offset + 2 <= font.len() {
                let codepoint = u16::from_le_bytes([font[offset], font[offset + 1]]);
                offset += 2;
                if codepoint == 0xffff {
                    break;
                }
                if codepoint == 0xfffe {
                    while offset + 2 <= font.len() {
                        let sequence = u16::from_le_bytes([font[offset], font[offset + 1]]);
                        offset += 2;
                        if sequence == 0xffff {
                            break;
                        }
                    }
                    break;
                }
                if codepoint as u32 == character as u32 {
                    return Some(glyph);
                }
            }
        }
        return None;
    }

    let mut offset = console.unicode_offset;
    for glyph in 0..console.glyph_count {
        while offset < font.len() && font[offset] != 0xff {
            if font[offset] == 0xfe {
                offset += 1;
                while offset < font.len() && font[offset] != 0xff {
                    offset += 1;
                }
                break;
            }
            let start = offset;
            while offset < font.len() && font[offset] != 0xfe && font[offset] != 0xff {
                offset += 1;
            }
            if core::str::from_utf8(&font[start..offset])
                .ok()?
                .chars()
                .any(|value| value == character)
            {
                return Some(glyph);
            }
        }
        if offset < font.len() && font[offset] == 0xff {
            offset += 1;
        } else {
            break;
        }
    }
    None
}

fn pixel(console: &Console, x: usize, y: usize, color: u32) {
    if !console.active || x >= console.width || y >= console.height {
        return;
    }
    let red = ((color >> 16) & 0xff) << console.red_shift;
    let green = ((color >> 8) & 0xff) << console.green_shift;
    let blue = (color & 0xff) << console.blue_shift;
    let value = red | green | blue;
    unsafe {
        ptr::write_volatile(
            console.address.add(y * console.pitch + x * 4).cast::<u32>(),
            value,
        );
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}
