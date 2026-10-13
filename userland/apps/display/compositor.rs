//! Compositor e window manager de demonstração.
//!
//! Os buffers de janela ficam num arena estático em `.bss`, nunca na pilha: um
//! canvas 320x200 ocupa 256 KiB e estouraria a stack de userspace de 128 KiB se
//! estivesse embutido numa variável local/array local.

use imagineos::display::{self, Framebuffer};
use imagineos::process;

use crate::apps;
use crate::theme::{self, Theme};
use crate::window::{Window, MAX_HEIGHT, MAX_WIDTH};

pub const MAX_WINDOWS: usize = 4;
/// Altura dos glifos da fonte bitmap compartilhada (8x8).
const FONT_GLYPH_HEIGHT: usize = 8;
/// Superfícies externas (clientes via protocolo de superfícies).
const MAX_EXTERNAL: usize = imagineos::abi::MAX_SURFACES;
const WINDOW_PIXELS: usize = MAX_WIDTH * MAX_HEIGHT;
/// Limite do buffer composto fora da tela. O QEMU/OVMF padrão usa 1024x768;
/// 1920x1080 também cabe com folga na RAM de teste (8 MiB em `.bss`).
const MAX_SCREEN_WIDTH: usize = 1920;
const MAX_SCREEN_HEIGHT: usize = 1080;
const SCREEN_PIXELS: usize = MAX_SCREEN_WIDTH * MAX_SCREEN_HEIGHT;

/// Backbuffer estático: a composição acontece aqui e o resultado é apresentado
/// depois, evitando limpar o framebuffer visível antes de redesenhar as janelas.
static mut BACKBUFFER: [u32; SCREEN_PIXELS] = [0; SCREEN_PIXELS];

/// Armazenamento estático dos canvases: 4 × 320 × 200 × 4 = 1 MiB em `.bss`.
/// Cada `Window` recebe um ponteiro exclusivo para um slot.
static mut CANVASES: [[u32; WINDOW_PIXELS]; MAX_WINDOWS] = [[0; WINDOW_PIXELS]; MAX_WINDOWS];

pub struct Compositor {
    framebuffer: Framebuffer,
    backbuffer: *mut u32,
    screen_pitch: usize,
    windows: [Option<Window>; MAX_WINDOWS],
    count: usize,
    focused: usize,
    pointer_x: i32,
    pointer_y: i32,
    dragging: Option<usize>,
    drag_offset_x: i32,
    drag_offset_y: i32,
    last_left: bool,
    /// Posição na tela de cada superfície externa (por id). O WM posiciona
    /// automaticamente: primeira superfície vira painel no topo, demais em
    /// cascata na área livre.
    external: [Option<ExternalSurface>; MAX_EXTERNAL],
    external_count: usize,
    /// Só redesenha quando houve um evento que alterou a tela. Redesenhar o
    /// framebuffer inteiro em cada iteração (100 Hz) satura o barramento e
    /// produz a piscada que fazia o servidor congelar após alguns segundos.
    dirty: bool,
    frame_number: u32,
    /// Tema carregado de `system/themes/default.theme` (cores, cursor e
    /// sprites de janela). Sem o disco, os padrões embutidos valem.
    theme: Theme,
    /// Retângulo ocupado pelo cursor no último quadro apresentado. Movimentos
    /// do ponteiro restauram só essa área no backbuffer e blitam apenas os
    /// scanlines afetados — recompor a tela inteira a cada pacote do mouse
    /// (4 MiB de escritas voláteis) era a causa do "lag" do cursor.
    cursor_rect: Rect,
    /// Verdadeiro quando `cursor_rect` ainda contém o cursor desenhado no
    /// backbuffer (o fundo salvo está atrás dele).
    cursor_present: bool,
    /// Movimento do ponteiro pendente (cursor precisa ser redesenhado sem
    /// recomposição completa).
    cursor_dirty: bool,
    /// Cópia do fundo por baixo do cursor desenhado (tamanho máximo do
    /// sprite: MAX_CURSOR_WIDTH x MAX_CURSOR_HEIGHT).
    cursor_saved: [u32; theme::MAX_CURSOR_WIDTH * theme::MAX_CURSOR_HEIGHT],
}

/// Retângulo de tela inclusive/exclusive, para atualizações parciais.
#[derive(Clone, Copy, Default)]
struct Rect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl Rect {
    fn empty() -> Self {
        Self {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    /// União conservadora de dois retângulos (para acumular dano).
    fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let left = self.x.min(other.x);
        let top = self.y.min(other.y);
        let right = (self.x + self.width).max(other.x + other.width);
        let bottom = (self.y + self.height).max(other.y + other.height);
        Rect {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        }
    }
}

/// Uma janela de um processo externo, composta pelo display server.
struct ExternalSurface {
    id: u32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    pitch: u32,
    /// Ponteiro para o buffer do cliente, mapeado no espaço do servidor pelo
    /// kernel em `surface_snapshot`. O compositor lê os pixels direto.
    pixels: *const u32,
}

impl Compositor {
    pub fn new(framebuffer: Framebuffer) -> Self {
        let backbuffer = core::ptr::addr_of_mut!(BACKBUFFER).cast::<u32>();
        Self {
            framebuffer,
            backbuffer,
            screen_pitch: MAX_SCREEN_WIDTH,
            windows: core::array::from_fn(|_| None),
            count: 0,
            focused: 0,
            pointer_x: 32,
            pointer_y: 32,
            dragging: None,
            drag_offset_x: 0,
            drag_offset_y: 0,
            last_left: false,
            external: core::array::from_fn(|_| None),
            external_count: 0,
            dirty: true,
            frame_number: 0,
            theme: Theme::builtin(),
            cursor_rect: Rect::empty(),
            cursor_present: false,
            cursor_dirty: false,
            cursor_saved: [0; theme::MAX_CURSOR_WIDTH * theme::MAX_CURSOR_HEIGHT],
        }
    }

    /// Carrega o tema do disco (CSS, cursor e sprites). Chamado uma vez
    /// depois de mapear a tela; falhas degradam para os padrões embutidos.
    pub fn load_theme(&mut self) {
        self.theme = Theme::load();
    }

    pub fn spawn_demo_windows(&mut self) {
        if self.framebuffer.width() == 0 || self.framebuffer.height() == 0 {
            return;
        }
        self.add_window(
            "Astrid terminal",
            36,
            42,
            260,
            150,
            self.theme.window.window,
            self.theme.window.desktop_accent,
            apps::draw_terminal,
        );
        self.add_window(
            "System monitor",
            510,
            390,
            194,
            108,
            self.theme.window.window_alt,
            self.theme.window.desktop_accent,
            apps::draw_monitor,
        );
    }

    fn add_window(
        &mut self,
        title: &str,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        background: u32,
        foreground: u32,
        draw: fn(&mut Window),
    ) {
        if self.count >= MAX_WINDOWS {
            return;
        }
        let slot = self.count;
        let canvas = unsafe { core::ptr::addr_of_mut!(CANVASES[slot]).cast::<u32>() };
        let mut window = Window::new(
            title,
            x,
            y,
            width,
            height,
            background,
            foreground,
            canvas,
            self.theme.window.border,
            self.theme.window.title_height,
        );
        window.z = slot as u32;
        draw(&mut window);
        self.windows[slot] = Some(window);
        self.count += 1;
        self.focused = slot;
    }

    pub fn run(&mut self) -> ! {
        // Primeiro frame: desenha o desktop e as janelas uma vez.
        self.compose();
        self.dirty = false;
        loop {
            self.poll_events();
            self.sync_external();
            if self.dirty {
                self.compose();
                self.dirty = false;
            } else if self.cursor_dirty {
                // Movimento de ponteiro sem outra mudança: em vez de recompor
                // a tela inteira (varredura de 1024x768 no backbuffer e cópia
                // de 3 MiB para o framebuffer), restaura apenas o retângulo
                // antigo do cursor, redesenha o cursor e bl os scanlines
                // afetados. É o caminho quente do WM — o cursor segue o mouse
                // com o custo de ~50 scanlines em vez de 768.
                self.update_cursor_only();
                self.cursor_dirty = false;
            }
            self.frame_number = self.frame_number.wrapping_add(1);
            if process::yield_now().is_err() {
                process::exit(1);
            }
        }
    }

    /// Atualiza só o cursor no backbuffer + framebuffer.
    fn update_cursor_only(&mut self) {
        if !self.cursor_present {
            self.compose();
            return;
        }
        // 1) Restaura o fundo salvo por baixo do cursor antigo.
        let saved = self.cursor_saved;
        let old = self.cursor_rect;
        if !old.is_empty() {
            self.blit_saved(&saved, &old);
        }
        // 2) Desenha o cursor na posição nova e salva o fundo por baixo.
        self.draw_cursor_and_save();
        // 3) Blita os scanlines da união dos dois retângulos no framebuffer.
        let damage = old.union(&self.cursor_rect);
        self.present_rect(&damage);
    }

    /// Restaura pixels de um backup (linha a linha, pitch do backup = largura
    /// do retângulo) para o backbuffer.
    fn blit_saved(&self, saved: &[u32], rect: &Rect) {
        if rect.is_empty() {
            return;
        }
        let width = rect.width as usize;
        for row in 0..rect.height as usize {
            for column in 0..width {
                let value = saved[row * width + column];
                let index = (rect.y as usize + row) * self.screen_pitch
                    + rect.x as usize
                    + column;
                unsafe { self.backbuffer.add(index).write(value) };
            }
        }
    }

    /// Copia do backbuffer para o framebuffer apenas as scanlines de `rect`.
    fn present_rect(&self, rect: &Rect) {
        if rect.is_empty() {
            return;
        }
        let screen_width = self.framebuffer.width() as i32;
        let screen_height = self.framebuffer.height() as i32;
        let left = rect.x.max(0);
        let top = rect.y.max(0);
        let right = (rect.x + rect.width).min(screen_width);
        let bottom = (rect.y + rect.height).min(screen_height);
        if left >= right || top >= bottom {
            return;
        }
        for row in top..bottom {
            let index = row as usize * self.screen_pitch + left as usize;
            let count = (right - left) as usize;
            let line = unsafe {
                core::slice::from_raw_parts(self.backbuffer.add(index), count)
            };
            self.framebuffer.copy_row(line, left as usize, row as usize);
        }
    }

    /// Sincroniza as superfícies externas com o kernel: entra as recém-
    /// registradas e remove as que o dono destruiu ou escondeu (o snapshot
    /// só devolve as visíveis; um processo morto tem os slots limpos no exit).
    fn sync_external(&mut self) {
        let mut snapshots = [imagineos::abi::SurfaceSnapshot::default(); MAX_EXTERNAL];
        let count = display::surfaces_snapshot(&mut snapshots);
        // Remove/renova entradas existentes.
        for index in 0..self.external_count {
            if self.external[index].is_none() {
                continue;
            }
            let entry = self.external[index].take().unwrap();
            match snapshots[..count].iter().find(|s| s.id == entry.id) {
                Some(snapshot) => {
                    let mut updated = entry;
                    updated.width = snapshot.width as i32;
                    updated.height = snapshot.height as i32;
                    updated.pitch = snapshot.pitch;
                    updated.pixels = snapshot.address as *const u32;
                    self.external[index] = Some(updated);
                }
                None => {
                    self.dirty = true;
                }
            }
        }
        // Compacta buracos.
        let mut write = 0;
        for read in 0..self.external_count {
            if let Some(entry) = self.external[read].take() {
                self.external[write] = Some(entry);
                write += 1;
            }
        }
        self.external_count = write;
        // Adiciona novas.
        for snapshot in &snapshots[..count] {
            if self.external.iter().take(self.external_count).any(|entry| {
                entry.as_ref().is_some_and(|existing| existing.id == snapshot.id)
            }) {
                continue;
            }
            if self.external_count >= MAX_EXTERNAL {
                break;
            }
            let (x, y) = self.external_position(self.external_count, snapshot);
            self.external[self.external_count] = Some(ExternalSurface {
                id: snapshot.id,
                x,
                y,
                width: snapshot.width as i32,
                height: snapshot.height as i32,
                pitch: snapshot.pitch,
                pixels: snapshot.address as *const u32,
            });
            self.external_count += 1;
            self.dirty = true;
        }
    }

    /// Posicionamento automático: a primeira superfície é a barra no topo
    /// (painel do EDE), as seguintes em cascata na área livre abaixo dela.
    fn external_position(&self, index: usize, snapshot: &imagineos::abi::SurfaceSnapshot) -> (i32, i32) {
        if index == 0 {
            return (0, 0);
        }
        let screen_w = self.framebuffer.width() as i32;
        let slot = (index - 1) % 6;
        let x = 40 + slot as i32 * 40;
        let y = 64 + slot as i32 * 32;
        (
            x.min((screen_w - snapshot.width as i32).max(0)),
            y,
        )
    }

    /// Id da superfície externa sob o ponto, se houver.
    fn external_at(&self, x: i32, y: i32) -> Option<u32> {
        for entry in self.external[..self.external_count].iter().flatten() {
            if x >= entry.x
                && x < entry.x + entry.width
                && y >= entry.y
                && y < entry.y + entry.height
            {
                return Some(entry.id);
            }
        }
        None
    }

    fn external_origin(&self, id: u32) -> (i32, i32) {
        self.external[..self.external_count]
            .iter()
            .flatten()
            .find(|entry| entry.id == id)
            .map_or((0, 0), |entry| (entry.x, entry.y))
    }

    fn poll_events(&mut self) {
        for _ in 0..64 {
            let event = match display::poll_input() {
                Ok(Some(event)) => event,
                Ok(None) | Err(_) => break,
            };
            match event.kind {
                imagineos::abi::INPUT_KEY => {
                    let key = char::from_u32(event.key).unwrap_or('\0');
                    if key == 'q' || key == 'Q' || key == '\u{f0007}' {
                        process::exit(0);
                    }
                    // Teclas de aplicação vão para a última superfície externa
                    // (a mais nova registrada fica em foco por padrão).
                    if self.external_count > 0 {
                        if let Some(last) = self.external[self.external_count - 1].as_ref() {
                            display::send_surface_event(last.id, imagineos::abi::SurfaceEvent {
                                surface: last.id,
                                kind: imagineos::abi::SURFACE_EVENT_KEY,
                                key: event.key,
                                ..Default::default()
                            });
                        }
                    }
                }
                imagineos::abi::INPUT_MOUSE_MOVE => {
                    let moved = self.pointer_x != event.x || self.pointer_y != event.y;
                    self.pointer_x = event.x;
                    self.pointer_y = event.y;
                    if moved {
                        // Só o cursor mudou: atualização por região suja, sem
                        // recompor as janelas.
                        self.cursor_dirty = true;
                    }
                    // Encaminha movimento à superfície externa sob o cursor.
                    if let Some(surface) = self.external_at(self.pointer_x, self.pointer_y) {
                        display::send_surface_event(surface, imagineos::abi::SurfaceEvent {
                            surface,
                            kind: imagineos::abi::SURFACE_EVENT_MOUSE_MOVE,
                            x: self.pointer_x - self.external_origin(surface).0,
                            y: self.pointer_y - self.external_origin(surface).1,
                            ..Default::default()
                        });
                    }
                    if let Some(index) = self.dragging {
                        if let Some(window) = self.windows[index].as_mut() {
                            window.move_to(
                                self.pointer_x - self.drag_offset_x,
                                self.pointer_y - self.drag_offset_y,
                                self.framebuffer.width() as i32,
                                self.framebuffer.height() as i32,
                            );
                        }
                    }
                }
                imagineos::abi::INPUT_MOUSE_BUTTON if event.button == 1 => {
                    let pressed = event.pressed != 0;
                    if pressed && !self.last_left {
                        // Superfície externa primeiro: sobre o painel, o dono
                        // recebe o clique em coordenadas locais.
                        if let Some(surface) = self.external_at(self.pointer_x, self.pointer_y) {
                            let (origin_x, origin_y) = self.external_origin(surface);
                            display::send_surface_event(surface, imagineos::abi::SurfaceEvent {
                                surface,
                                kind: imagineos::abi::SURFACE_EVENT_MOUSE_BUTTON,
                                x: self.pointer_x - origin_x,
                                y: self.pointer_y - origin_y,
                                button: event.button,
                                pressed: event.pressed,
                                ..Default::default()
                            });
                        } else {
                            self.begin_drag_or_focus();
                        }
                        self.dirty = true;
                    } else if !pressed && self.last_left {
                        self.dragging = None;
                        self.dirty = true;
                        if let Some(surface) = self.external_at(self.pointer_x, self.pointer_y) {
                            let (origin_x, origin_y) = self.external_origin(surface);
                            display::send_surface_event(surface, imagineos::abi::SurfaceEvent {
                                surface,
                                kind: imagineos::abi::SURFACE_EVENT_MOUSE_BUTTON,
                                x: self.pointer_x - origin_x,
                                y: self.pointer_y - origin_y,
                                button: event.button,
                                pressed: 0,
                                ..Default::default()
                            });
                        }
                    }
                    self.last_left = pressed;
                }
                _ => {}
            }
        }
    }

    fn begin_drag_or_focus(&mut self) {
        // Procura de cima para baixo pela janela sob o cursor.
        let mut selected = None;
        for index in (0..self.count).rev() {
            if self.windows[index]
                .as_ref()
                .is_some_and(|window| window.contains(self.pointer_x, self.pointer_y))
            {
                selected = Some(index);
                break;
            }
        }
        let Some(index) = selected else {
            return;
        };
        self.focused = index;
        if let Some(window) = self.windows[index].as_ref() {
            if window.holds_title(self.pointer_x, self.pointer_y) {
                self.dragging = Some(index);
                self.drag_offset_x = self.pointer_x - window.x;
                self.drag_offset_y = self.pointer_y - window.y;
            }
        }
        // Traz a janela focada para frente, sem reordenar o array.
        if let Some(window) = self.windows[index].as_mut() {
            window.z = self.frame_number;
        }
        self.dirty = true;
    }

    fn compose(&mut self) {
        let background = self.framebuffer.color(self.theme.window.desktop);
        self.screen_fill(
            0,
            0,
            self.framebuffer.width() as i32,
            self.framebuffer.height() as i32,
            background,
        );
        self.draw_desktop_bar();

        // Ordem simples por z: até quatro janelas, então uma ordenação
        // estável em stack local é pequena (índices apenas, não os canvases).
        let mut order = [0usize; MAX_WINDOWS];
        for index in 0..self.count {
            order[index] = index;
        }
        // Ordenação por inserção: `core` não expõe `slice::sort_by_key` em
        // `no_std`, e são no máximo quatro janelas.
        for index in 1..self.count {
            let mut cursor = index;
            while cursor > 0 {
                let left_z = self.windows[order[cursor - 1]].as_ref().map_or(0, |w| w.z);
                let right_z = self.windows[order[cursor]].as_ref().map_or(0, |w| w.z);
                if left_z <= right_z {
                    break;
                }
                order.swap(cursor - 1, cursor);
                cursor -= 1;
            }
        }
        for &index in &order[..self.count] {
            if let Some(window) = self.windows[index].as_ref() {
                self.draw_window(window, index == self.focused);
            }
        }
        // Superfícies externas: desenha após as janelas internas (o painel do
        // EDE fica na frente do desktop, janelas de clientes na área livre).
        for entry in self.external[..self.external_count].iter().flatten() {
            self.draw_external(entry);
        }
        self.draw_cursor_and_save();
        // Presenta uma única imagem composta, em vez de limpar e redesenhar a
        // tela diretamente. Isso elimina o flash do desktop entre janelas.
        self.present_rect(&Rect {
            x: 0,
            y: 0,
            width: self.framebuffer.width() as i32,
            height: self.framebuffer.height() as i32,
        });
        self.cursor_present = true;
    }

    fn draw_desktop_bar(&self) {
        let bar = self.framebuffer.color(self.theme.window.border_color);
        let accent = self.framebuffer.color(self.theme.window.desktop_accent);
        let width = self.framebuffer.width() as i32;
        self.screen_fill(0, 0, width, 24, bar);
        self.screen_fill(0, 23, width, 1, accent);
        // O framebuffer do kernel não tem fonte pública, então os títulos de
        // desktop são pintados pelos canvases das janelas; a barra é uma faixa.
    }

    fn draw_window(&self, window: &Window, focused: bool) {
        let window_theme = &self.theme.window;
        let border = self.framebuffer.color(window_theme.border_color);
        let title = self.framebuffer.color(if focused {
            window_theme.title_active
        } else {
            window_theme.title_inactive
        });
        let title_text = self.framebuffer.color(window_theme.title_text);
        let content_x = window.x + window.border;
        let content_y = window.y + window.title_height;
        let outer_width = window.width + 2 * window.border;
        let outer_height = window.height + window.title_height + window.border;

        self.screen_fill(window.x, window.y, outer_width, outer_height, border);
        self.screen_fill(content_x, window.y, window.width, window.title_height, title);

        // Botões fechar/minimizar — decoração visual neste protótipo.
        let button = window_theme.button_size;
        self.screen_fill(
            window.x + outer_width - button - 3,
            window.y + (window.title_height - button) / 2,
            button,
            button,
            self.framebuffer.color(window_theme.button_close),
        );
        self.screen_fill(
            window.x + outer_width - button * 2 - 7,
            window.y + (window.title_height - button) / 2,
            button,
            button,
            self.framebuffer.color(window_theme.button_minimize),
        );

        for row in 0..window.height as usize {
            for column in 0..window.width as usize {
                let source = row * window.canvas_pitch + column;
                let pixel = unsafe { window.canvas.add(source).read() };
                self.screen_pixel(content_x + column as i32, content_y + row as i32, pixel);
            }
        }
        // Título com a fonte bitmap (desenhado no backbuffer).
        draw_text(
            self,
            content_x + 8,
            window.y + (window.title_height - FONT_GLYPH_HEIGHT as i32) / 2,
            window.title(),
            title_text,
        );
    }

    /// Copia o buffer do cliente (via mapeamento HHDM do kernel) para o
    /// backbuffer, sem moldura: o painel e os clientes desenham o próprio
    /// visual completo.
    fn draw_external(&self, surface: &ExternalSurface) {
        let width = surface.width.min(self.framebuffer.width() as i32);
        let height = surface.height.min(self.framebuffer.height() as i32);
        for row in 0..height as usize {
            for column in 0..width as usize {
                let pixel = unsafe {
                    surface.pixels.add(row * surface.pitch as usize + column).read()
                };
                self.screen_pixel(surface.x + column as i32, surface.y + row as i32, pixel);
            }
        }
    }

    fn screen_pixel(&self, x: i32, y: i32, color: u32) {
        if x < 0 || y < 0 || x >= self.framebuffer.width() as i32 || y >= self.framebuffer.height() as i32 {
            return;
        }
        let index = y as usize * self.screen_pitch + x as usize;
        unsafe { self.backbuffer.add(index).write(color) };
    }

    fn screen_fill(&self, x: i32, y: i32, width: i32, height: i32, color: u32) {
        let left = x.max(0);
        let top = y.max(0);
        let right = x.saturating_add(width).min(self.framebuffer.width() as i32);
        let bottom = y.saturating_add(height).min(self.framebuffer.height() as i32);
        if left >= right || top >= bottom {
            return;
        }
        for row in top..bottom {
            for column in left..right {
                let index = row as usize * self.screen_pitch + column as usize;
                unsafe { self.backbuffer.add(index).write(color) };
            }
        }
    }

    /// Desenha o cursor na posição atual e salva o fundo por baixo dele.
    ///
    /// A seta é desenhada com primitivas (pixels), nas cores do CSS
    /// (`#cursor { background-color; color }`). O fundo salvo permite que um
    /// movimento do ponteiro restaure apenas o retângulo antigo — é o que
    /// tira o "lag" do cursor sem recompor a tela.
    fn draw_cursor_and_save(&mut self) {
        let window_theme = &self.theme.window;
        let screen_width = self.framebuffer.width() as i32;
        let screen_height = self.framebuffer.height() as i32;
        let size = window_theme.cursor_size;
        // Recorta o retângulo na tela e salva o fundo.
        let left = self.pointer_x.max(0);
        let top = self.pointer_y.max(0);
        let right = (self.pointer_x + size).min(screen_width);
        let bottom = (self.pointer_y + size).min(screen_height);
        if left < right && top < bottom {
            for row in top..bottom {
                for column in left..right {
                    let index = row as usize * self.screen_pitch + column as usize;
                    let value = unsafe { self.backbuffer.add(index).read() };
                    let slot_row = (row - top) as usize;
                    let slot_column = (column - left) as usize;
                    self.cursor_saved[slot_row * theme::MAX_CURSOR_WIDTH + slot_column] = value;
                }
            }
        }
        self.cursor_rect = Rect {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        };
        // Seta clássica: contorno escuro no perímetro, corpo claro no miolo.
        let outline = self.framebuffer.color(window_theme.cursor_outline);
        let body = self.framebuffer.color(window_theme.cursor_color);
        for row in 0..size {
            let line_width = (row / 2 + 1).min(size - row / 2);
            for column in 0..line_width {
                self.screen_pixel(self.pointer_x + column, self.pointer_y + row, outline);
            }
        }
        for row in 1..size - 2 {
            let line_width = (row / 2).max(1);
            for column in 0..line_width {
                self.screen_pixel(self.pointer_x + column, self.pointer_y + row, body);
            }
        }
        self.cursor_present = true;
    }
}
/// Desenha texto no backbuffer usando a fonte bitmap 8x8 compartilhada
/// (crate `imagineos`), igual à dos clientes de superfície.
fn draw_text(compositor: &Compositor, mut x: i32, y: i32, text: &str, color: u32) {
    for character in text.chars() {
        let bitmap = imagineos::font::glyph(character);
        for (row, bits) in bitmap.iter().enumerate() {
            for column in 0..8 {
                if bits & (0x80 >> column) != 0 {
                    compositor.screen_pixel(x + column as i32, y + row as i32, color);
                }
            }
        }
        x += 8 + 1;
    }
}
