//! Modelo de janela: um buffer de pixels com uma moldura e um painel de título.
//!
//! Cada janela desenha no seu próprio buffer; o compositor copia o buffer para
//! a tela. Esse desenho em dois passos é o que permite mover ou sobrepor
//! janelas sem que cada aplicação precise redesenhar.

/// Tamanho máximo de uma janela, em pixels.
pub const MAX_WIDTH: usize = 320;
pub const MAX_HEIGHT: usize = 200;

/// Uma janela com posição, tamanho e buffer próprios.
pub struct Window {
    pub title: [u8; 32],
    pub title_length: usize,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// Ponteiro para o slot de canvas pertencente ao compositor. O buffer fica
    /// em `.bss` estático (256 KiB por janela), nunca na pilha do processo.
    /// O display server é monothreaded, então só há um escritor por buffer.
    pub canvas: *mut u32,
    pub canvas_pitch: usize,
    /// Cores no formato nativo do framebuffer.
    pub background: u32,
    pub foreground: u32,
    /// Ordem de empilhamento: maior = mais à frente.
    pub z: u32,
    /// Espessura da moldura (tema: `#border { border-width: Npx }`).
    pub border: i32,
    /// Altura da barra de título (tema: `#titlebar { height: Npx }`).
    pub title_height: i32,
}

impl Window {
    /// Cria uma janela vazia. O título é copiado; o tamanho é recortado nos
    /// limites do buffer.
    pub fn new(
        title: &str,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        background: u32,
        foreground: u32,
        canvas: *mut u32,
        border: i32,
        title_height: i32,
    ) -> Self {
        let mut title_bytes = [0u8; 32];
        let length = title.len().min(title_bytes.len());
        title_bytes[..length].copy_from_slice(&title.as_bytes()[..length]);
        let width = width.clamp(1, MAX_WIDTH as i32);
        let height = height.clamp(1, MAX_HEIGHT as i32);
        let canvas_pitch = MAX_WIDTH;
        let required = canvas_pitch.saturating_mul(MAX_HEIGHT);
        // SAFETY: o ponteiro é um slot exclusivo do arena estático do
        // compositor, com `MAX_WIDTH * MAX_HEIGHT` pixels.
        unsafe {
            for index in 0..required {
                canvas.add(index).write(background);
            }
        }
        Self {
            title: title_bytes,
            title_length: length,
            x,
            y,
            width,
            height,
            canvas,
            canvas_pitch,
            background,
            foreground,
            z: 0,
            border: border.max(0),
            title_height: title_height.max(1),
        }
    }

    pub fn title(&self) -> &str {
        core::str::from_utf8(&self.title[..self.title_length]).unwrap_or("")
    }

    /// Limpa o conteúdo com a cor de fundo.
    pub fn clear(&mut self) {
        for row in 0..self.height as usize {
            for column in 0..self.width as usize {
                unsafe {
                    self.canvas.add(row * self.canvas_pitch + column).write(self.background);
                }
            }
        }
    }

    /// Escreve um pixel no conteúdo, ignorando coordenadas fora da janela.
    pub fn pixel(&mut self, x: i32, y: i32, color: u32) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        let index = y as usize * self.canvas_pitch + x as usize;
        // SAFETY: x/y foram limitados às dimensões do canvas.
        unsafe { self.canvas.add(index).write(color) }
    }

    pub fn fill(&mut self, x: i32, y: i32, width: i32, height: i32, color: u32) {
        let left = x.max(0);
        let top = y.max(0);
        let right = x.saturating_add(width).min(self.width);
        let bottom = y.saturating_add(height).min(self.height);
        if left >= right || top >= bottom {
            return;
        }
        for row in top..bottom {
            for column in left..right {
                let index = row as usize * self.canvas_pitch + column as usize;
                // SAFETY: row/column estão limitados às dimensões da janela.
                unsafe { self.canvas.add(index).write(color) }
            }
        }
    }

    pub fn outline(&mut self, x: i32, y: i32, width: i32, height: i32, thickness: i32, color: u32) {
        self.fill(x, y, width, thickness, color);
        self.fill(x, y + height - thickness, width, thickness, color);
        self.fill(x, y, thickness, height, color);
        self.fill(x + width - thickness, y, thickness, height, color);
    }

    /// Escreve texto usando a fonte de 8x8 compartilhada.
    pub fn text(&mut self, x: i32, y: i32, text: &str, color: u32) {
        const GLYPH_WIDTH: usize = 8;
        const GLYPH_HEIGHT: usize = 8;
        let origin_x = x;
        let mut cursor_x = x;
        let mut cursor_y = y;
        for character in text.chars() {
            if character == '\n' {
                cursor_x = origin_x;
                cursor_y += GLYPH_HEIGHT as i32 + 2;
                continue;
            }
            if cursor_x + GLYPH_WIDTH as i32 > self.width {
                cursor_x = origin_x;
                cursor_y += GLYPH_HEIGHT as i32 + 2;
            }
            if cursor_y + GLYPH_HEIGHT as i32 > self.height {
                break;
            }
            self.glyph(cursor_x, cursor_y, character, color);
            cursor_x += GLYPH_WIDTH as i32 + 1;
        }
    }

    /// Desenha um caractere a partir da tabela de glifos compartilhada.
    fn glyph(&mut self, x: i32, y: i32, character: char, color: u32) {
        const GLYPH_WIDTH: usize = 8;
        const GLYPH_HEIGHT: usize = 8;
        let bitmap = imagineos::font::glyph(character);
        for (row, bits) in bitmap.iter().enumerate() {
            for column in 0..GLYPH_WIDTH {
                if bits & (0x80 >> column) != 0 {
                    self.pixel(x + column as i32, y + row as i32, color);
                }
            }
        }
    }

    /// O ponteiro está sobre o painel de título desta janela?
    pub fn holds_title(&self, x: i32, y: i32) -> bool {
        x >= self.x
            && x < self.x + self.width + 2 * self.border
            && y >= self.y
            && y < self.y + self.title_height
    }

    /// O ponteiro está em qualquer parte desta janela?
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x
            && x < self.x + self.width + 2 * self.border
            && y >= self.y
            && y < self.y + self.height + self.title_height + self.border
    }

    /// Move a janela, mantendo-a visível nas dimensões da tela.
    pub fn move_to(&mut self, x: i32, y: i32, screen_width: i32, screen_height: i32) {
        let width = self.width + 2 * self.border;
        let height = self.height + self.title_height + self.border;
        self.x = x.clamp(0, (screen_width - width).max(0));
        self.y = y.clamp(0, (screen_height - height).max(0));
    }
}
