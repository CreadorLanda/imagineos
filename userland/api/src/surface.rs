//! Protocolo de superfícies: como um processo vira uma "janela" no desktop.
//!
//! O fluxo de um cliente (painel, terminal, calculadora):
//!
//! 1. [`Surface::create`] — registra a superfície e recebe um buffer de
//!    pixels mapeado no próprio espaço do processo;
//! 2. desenha no buffer com [`Surface::fill`]/[`Surface::pixel`]/[`Surface::text`];
//! 3. [`Surface::present`] — torna visível e publica o frame;
//! 4. [`poll_event`] — lê os eventos que o compositor encaminha;
//! 5. quando algo muda, redesenha e chama [`Surface::flush`].
//!
//! O buffer é memória compartilhada com o compositor: o que o cliente escreve
//! é o que aparece na tela. Não há cópia pelo kernel no caminho do desenho —
//! apenas o `flush` marca o frame como pronto.

use crate::abi::{SurfaceEvent, SurfaceInfo, SURFACE_MAX_HEIGHT, SURFACE_MAX_WIDTH};
use crate::{checked, syscall, Error, Result};

/// Uma superfície de cliente com buffer mapeado.
pub struct Surface {
    id: u32,
    width: usize,
    height: usize,
    pitch: usize,
    pixels: *mut u32,
}

impl Surface {
    /// Registra a superfície no kernel e mapeia o buffer.
    pub fn create(width: usize, height: usize) -> Result<Self> {
        if width == 0
            || height == 0
            || width > SURFACE_MAX_WIDTH as usize
            || height > SURFACE_MAX_HEIGHT as usize
        {
            return Err(Error::INVALID_ARGUMENT);
        }
        let mut info = SurfaceInfo::default();
        checked(syscall::invoke(
            crate::abi::SYS_SURFACE_CREATE,
            [(&mut info as *mut SurfaceInfo) as u64, 0, 0, 0, 0, 0],
        ))?;
        if info.id == 0 && info.address == 0 {
            return Err(Error::INVALID_ARGUMENT);
        }
        Ok(Self {
            id: info.id,
            width,
            height,
            pitch: info.pitch as usize,
            pixels: info.address as *mut u32,
        })
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// Escreve um pixel, ignorando coordenadas fora da superfície.
    pub fn pixel(&self, x: i32, y: i32, color: u32) {
        if x < 0 || y < 0 {
            return;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= self.width || y >= self.height {
            return;
        }
        unsafe {
            core::ptr::write_volatile(self.pixels.add(y * self.pitch + x), color);
        }
    }

    /// Preenche um retângulo com recorte nas bordas.
    pub fn fill(&self, x: i32, y: i32, width: i32, height: i32, color: u32) {
        let left = x.max(0);
        let top = y.max(0);
        let right = x.saturating_add(width).min(self.width as i32);
        let bottom = y.saturating_add(height).min(self.height as i32);
        if left >= right || top >= bottom {
            return;
        }
        for row in top..bottom {
            for column in left..right {
                self.pixel(column, row, color);
            }
        }
    }

    /// Limpa com uma cor sólida.
    pub fn clear(&self, color: u32) {
        self.fill(0, 0, self.width as i32, self.height as i32, color);
    }

    /// Moldura de um retângulo.
    pub fn outline(&self, x: i32, y: i32, width: i32, height: i32, thickness: i32, color: u32) {
        self.fill(x, y, width, thickness, color);
        self.fill(x, y + height - thickness, width, thickness, color);
        self.fill(x, y, thickness, height, color);
        self.fill(x + width - thickness, y, thickness, height, color);
    }

    /// Texto com a fonte bitmap 8x8 compartilhada.
    pub fn text(&self, x: i32, y: i32, text: &str, color: u32) {
        let mut cursor_x = x;
        let mut cursor_y = y;
        for character in text.chars() {
            if character == '\n' {
                cursor_x = x;
                cursor_y += crate::font::GLYPH_HEIGHT as i32 + 2;
                continue;
            }
            if cursor_x + crate::font::GLYPH_WIDTH as i32 > self.width as i32 {
                cursor_x = x;
                cursor_y += crate::font::GLYPH_HEIGHT as i32 + 2;
            }
            if cursor_y + crate::font::GLYPH_HEIGHT as i32 > self.height as i32 {
                break;
            }
            self.glyph(cursor_x, cursor_y, character, color);
            cursor_x += crate::font::GLYPH_WIDTH as i32 + 1;
        }
    }

    fn glyph(&self, x: i32, y: i32, character: char, color: u32) {
        let bitmap = crate::font::glyph(character);
        for (row, bits) in bitmap.iter().enumerate() {
            for column in 0..crate::font::GLYPH_WIDTH {
                if bits & (0x80 >> column) != 0 {
                    self.pixel(x + column as i32, y + row as i32, color);
                }
            }
        }
    }

    /// Torna a superfície visível (e publica o frame atual).
    pub fn present(&self) -> Result<()> {
        checked(syscall::invoke(
            crate::abi::SYS_SURFACE_PRESENT,
            [self.id as u64, 1, 0, 0, 0, 0],
        ))
        .map(|_| ())
    }

    /// Esconde a superfície sem destruí-la.
    pub fn hide(&self) -> Result<()> {
        checked(syscall::invoke(
            crate::abi::SYS_SURFACE_PRESENT,
            [self.id as u64, 0, 0, 0, 0, 0],
        ))
        .map(|_| ())
    }

    /// Publica o frame atual (o compositor recopia na próxima composição).
    pub fn flush(&self) -> Result<()> {
        checked(syscall::invoke(
            crate::abi::SYS_SURFACE_FLUSH,
            [self.id as u64, 0, 0, 0, 0, 0],
        ))
        .map(|_| ())
    }
}

/// Remove a superfície (sai da tela e libera o slot).
pub fn destroy(surface: &Surface) -> Result<()> {
    checked(syscall::invoke(
        crate::abi::SYS_SURFACE_DESTROY,
        [surface.id as u64, 0, 0, 0, 0, 0],
    ))
    .map(|_| ())
}

/// Retira um evento dirigido às superfícies deste processo. `Ok(None)` quando
/// não há evento; o chamador normalmente repassa o tempo para `yield`.
pub fn poll_event() -> Result<Option<SurfaceEvent>> {
    let mut event = SurfaceEvent::default();
    let result = checked(syscall::invoke(
        crate::abi::SYS_SURFACE_POLL,
        [(&mut event as *mut SurfaceEvent) as u64, 0, 0, 0, 0, 0],
    ))?;
    if result == 0 {
        Ok(None)
    } else {
        Ok(Some(event))
    }
}

/// Constantes reexportadas para o cliente tratar eventos sem depender da ABI.
pub use crate::abi::{
    SURFACE_EVENT_HIDDEN as EVENT_HIDDEN, SURFACE_EVENT_KEY as EVENT_KEY,
    SURFACE_EVENT_MOUSE_BUTTON as EVENT_MOUSE_BUTTON, SURFACE_EVENT_MOUSE_MOVE as EVENT_MOUSE_MOVE,
};
