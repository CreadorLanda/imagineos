//! Acesso ao framebuffer e aos eventos de entrada.
//!
//! Esta é a base do servidor de display: um processo comum (sem privilégio
//! especial) mapeia a memória de vídeo e lê os eventos de teclado e mouse.
//! Quem decide quando a tela deixa de pertencer ao console do kernel é
//! [`release_console`] — enquanto ela não é chamada, o console do kernel
//! continua desenhando e os eventos seguem o caminho normal.

use crate::abi::{FramebufferInfo, InputEvent};
use crate::{checked, syscall, Error, Result};

/// Um framebuffer mapeado no espaço do processo.
///
/// O valor é um descritor, não uma cópia: `pixel` escreve direto na memória de
/// vídeo. Não há sincronização com outros clientes — num servidor de display,
/// só um processo deve desenhar (o próprio servidor), que compõe as janelas.
pub struct Framebuffer {
    address: *mut u32,
    width: usize,
    height: usize,
    pitch_words: usize,
    red_shift: u8,
    green_shift: u8,
    blue_shift: u8,
}

impl Framebuffer {
    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// Converte um RGB de 24 bits no valor nativo do framebuffer.
    pub fn color(&self, rgb: u32) -> u32 {
        let red = ((rgb >> 16) & 0xff) << self.red_shift;
        let green = ((rgb >> 8) & 0xff) << self.green_shift;
        let blue = (rgb & 0xff) << self.blue_shift;
        red | green | blue
    }

    /// Escreve um pixel, ignorando coordenadas fora da tela.
    pub fn pixel(&self, x: i32, y: i32, color: u32) {
        if x < 0 || y < 0 {
            return;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= self.width || y >= self.height {
            return;
        }
        unsafe {
            core::ptr::write_volatile(self.address.add(y * self.pitch_words + x), color);
        }
    }

    /// Preenche um retângulo, recortando nas bordas da tela.
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
                unsafe {
                    core::ptr::write_volatile(
                        self.address
                            .add(row as usize * self.pitch_words + column as usize),
                        color,
                    );
                }
            }
        }
    }

    /// Copia um canvas de memória normal para o framebuffer com escritas
    /// voláteis. `source_pitch_words` é a largura física do buffer de origem.
    /// Usado pelo display server para compor fora da tela e só então apresentar
    /// o frame completo, em vez de limpar/desenhar diretamente a cada janela.
    pub fn copy_from(&self, source: &[u32], source_pitch_words: usize) {
        let rows = self.height;
        let columns = self.width;
        for y in 0..rows {
            for x in 0..columns {
                let source_index = y * source_pitch_words + x;
                if let Some(&pixel) = source.get(source_index) {
                    unsafe {
                        core::ptr::write_volatile(
                            self.address.add(y * self.pitch_words + x),
                            pixel,
                        );
                    }
                }
            }
        }
    }

    /// Copia uma linha contígua do backbuffer para o framebuffer. Usado pelo
    /// compositor para apresentar apenas as regiões sujas (movimento de
    /// cursor), sem reescrever a tela inteira.
    pub fn copy_row(&self, source: &[u32], column: usize, row: usize) {
        if row >= self.height {
            return;
        }
        for (index, &pixel) in source.iter().enumerate() {
            if column + index >= self.width {
                break;
            }
            unsafe {
                core::ptr::write_volatile(
                    self.address.add(row * self.pitch_words + column + index),
                    pixel,
                );
            }
        }
    }

    /// Moldura de um retângulo, útil para desenhar bordas de janela.
    pub fn outline(&self, x: i32, y: i32, width: i32, height: i32, thickness: i32, color: u32) {
        self.fill(x, y, width, thickness, color);
        self.fill(x, y + height - thickness, width, thickness, color);
        self.fill(x, y, thickness, height, color);
        self.fill(x + width - thickness, y, thickness, height, color);
    }
}

/// Consulta o framebuffer ativo sem mapeá-lo.
pub fn framebuffer_info() -> Result<FramebufferInfo> {
    let mut info = FramebufferInfo::default();
    checked(syscall::invoke(
        crate::abi::SYS_FB_INFO,
        [(&mut info as *mut FramebufferInfo) as u64, 0, 0, 0, 0, 0],
    ))?;
    Ok(info)
}

/// Mapeia o framebuffer no processo atual e devolve o descritor de desenho.
pub fn map_framebuffer() -> Result<Framebuffer> {
    let mut info = FramebufferInfo::default();
    checked(syscall::invoke(
        crate::abi::SYS_FB_MAP,
        [(&mut info as *mut FramebufferInfo) as u64, 0, 0, 0, 0, 0],
    ))?;
    if info.address == 0 || info.width == 0 || info.height == 0 || info.pitch < info.width * 4 {
        return Err(Error::INVALID_ARGUMENT);
    }
    Ok(Framebuffer {
        address: info.address as *mut u32,
        width: info.width as usize,
        height: info.height as usize,
        pitch_words: info.pitch as usize / 4,
        red_shift: info.red_shift,
        green_shift: info.green_shift,
        blue_shift: info.blue_shift,
    })
}

/// O servidor de display assume a tela: o console do kernel para de desenhar
/// nela e os eventos de entrada passam a vir de [`poll_input`].
pub fn release_console() -> Result<()> {
    checked(syscall::invoke(crate::abi::SYS_CONSOLE_RELEASE, [0; 6])).map(|_| ())
}

/// Retira um evento de entrada, se houver.
pub fn poll_input() -> Result<Option<InputEvent>> {
    let mut event = InputEvent::default();
    let result = checked(syscall::invoke(
        crate::abi::SYS_INPUT_POLL,
        [(&mut event as *mut InputEvent) as u64, 0, 0, 0, 0, 0],
    ))?;
    if result == 0 {
        Ok(None)
    } else {
        Ok(Some(event))
    }
}

/// Posição atual do ponteiro, sem consumir eventos.
pub fn pointer_position() -> Result<(i32, i32)> {
    let packed = checked(syscall::invoke(crate::abi::SYS_POINTER_POSITION, [0; 6]))?;
    Ok(((packed >> 32) as u32 as i32, packed as u32 as i32))
}

/// Lista as superfícies visíveis registradas no kernel. O servidor de display
/// chama em cada quadro para saber o que compor; os buffers voltam mapeados
/// no espaço do processo chamador (`SurfaceSnapshot.address`).
pub fn surfaces_snapshot(output: &mut [crate::abi::SurfaceSnapshot]) -> usize {
    let capacity = output.len();
    if capacity == 0 {
        return 0;
    }
    let result = checked(syscall::invoke(
        crate::abi::SYS_SURFACE_SNAPSHOT,
        [output.as_mut_ptr() as u64, capacity as u64, 0, 0, 0, 0],
    ));
    match result {
        Ok(count) => count as usize,
        Err(_) => 0,
    }
}

/// Entrega um evento à superfície indicada (roteado pelo kernel ao dono).
pub fn send_surface_event(surface_id: u32, mut event: crate::abi::SurfaceEvent) {
    event.surface = surface_id;
    let _ = checked(syscall::invoke(
        crate::abi::SYS_SURFACE_SEND_EVENT,
        [(&mut event as *mut crate::abi::SurfaceEvent) as u64, 0, 0, 0, 0, 0],
    ));
}
