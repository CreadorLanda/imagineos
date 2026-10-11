//! Fila de eventos de entrada para o servidor de display.
//!
//! O console do kernel lê o teclado diretamente (`crate::keyboard`), mas um
//! servidor de janelas precisa de um fluxo de eventos com um dono só. Esta
//! fila é um buffer circular alimentado por [`push_key`]/[`push_pointer`] e
//! drenada pela syscall `input_poll`. Enquanto houver um "dono" registrado
//! (o servidor de display), a leitura direta do teclado pelo console é
//! desviada para cá — assim o mesmo caractere não é consumido duas vezes.

use core::cell::UnsafeCell;

use imagineos_abi::{InputEvent, INPUT_KEY, INPUT_MOUSE_BUTTON, INPUT_MOUSE_MOVE};

const CAPACITY: usize = 128;

struct Queue {
    events: [InputEvent; CAPACITY],
    head: usize,
    tail: usize,
    length: usize,
    /// Quando verdadeiro, o servidor de display é o dono dos eventos.
    captured: bool,
    /// Posição e estado do ponteiro, mantidos aqui para montar os eventos de
    /// movimento//botão com um lugar só de verdade.
    pointer_x: i32,
    pointer_y: i32,
    /// Limites da área visível, registrados quando o framebuffer é mapeado.
    /// Antes disso, um máximo generoso evita acumular posição sem fim.
    pointer_max_x: i32,
    pointer_max_y: i32,
    left_pressed: bool,
    right_pressed: bool,
}

impl Queue {
    const fn new() -> Self {
        Self {
            events: [EMPTY; CAPACITY],
            head: 0,
            tail: 0,
            length: 0,
            captured: false,
            pointer_x: 0,
            pointer_y: 0,
            pointer_max_x: MAX_POINTER - 1,
            pointer_max_y: MAX_POINTER - 1,
            left_pressed: false,
            right_pressed: false,
        }
    }
}

/// Evento vazio, usado para construir variantes com `..EMPTY`.
const EMPTY: InputEvent = InputEvent {
    kind: 0,
    key: 0,
    x: 0,
    y: 0,
    button: 0,
    pressed: 0,
};

struct SharedQueue(UnsafeCell<Queue>);
unsafe impl Sync for SharedQueue {}
static QUEUE: SharedQueue = SharedQueue(UnsafeCell::new(Queue::new()));

fn queue() -> &'static mut Queue {
    unsafe { &mut *QUEUE.0.get() }
}

fn push(event: InputEvent) {
    let queue = queue();
    // Coalescing de movimento: se o último evento já for `MOUSE_MOVE`, ele é
    // substituído. O PS/2 entrega dezenas de pacotes por segundo; empilhar um
    // evento por pacote enchia a fila e o WM desenhava vários quadros atrás da
    // posição real do cursor (o "lag" e o salto para posições antigas).
    if event.kind == INPUT_MOUSE_MOVE
        && queue.length != 0
        && queue.events[(queue.tail + CAPACITY - 1) % CAPACITY].kind == INPUT_MOUSE_MOVE
    {
        queue.events[(queue.tail + CAPACITY - 1) % CAPACITY] = event;
        return;
    }
    if queue.length == CAPACITY {
        // Fila cheia: descarta o evento mais antigo, como um servidor de
        // display faria com um cliente lento. Perder um clique é preferível a
        // travar a entrada.
        queue.head = (queue.head + 1) % CAPACITY;
        queue.length -= 1;
    }
    queue.events[queue.tail] = event;
    queue.tail = (queue.tail + 1) % CAPACITY;
    queue.length += 1;
}

/// Toma posse dos eventos de entrada para o servidor de display.
pub fn capture() {
    let queue = queue();
    queue.captured = true;
    queue.head = 0;
    queue.tail = 0;
    queue.length = 0;
}

/// Devolve a posse ao console do kernel.
pub fn release() {
    queue().captured = false;
}

/// Verdadeiro quando o servidor de display é o dono dos eventos.
pub fn is_captured() -> bool {
    queue().captured
}

/// Registra um caractere de teclado.
///
/// Códigos acima de `0xffff` (as teclas especiais do driver, como setas) são
/// preservados no campo `key` como estão, para que o servidor possa tratá-los.
pub fn push_key(character: char) {
    push(InputEvent {
        kind: INPUT_KEY,
        key: character as u32,
        ..EMPTY
    });
}

/// Registra movimento do ponteiro.
///
/// A posição é mantida dentro de [0, MAX_POINTER); se um limite de tela ainda
/// não foi registrado (`clamp_pointer`), assumimos um máximo generoso. Sem o
/// limite superior, o ponteiro acumula posição fora da tela quando o usuário
/// empurra o mouse contra a borda: o cursor parece "agarrar", e é preciso
/// retroceder todo o movimento acumulado antes de ele reaparecer.
const MAX_POINTER: i32 = 4096;
pub fn push_pointer(delta_x: i32, delta_y: i32) {
    let queue = queue();
    queue.pointer_x = (queue.pointer_x + delta_x).clamp(0, queue.pointer_max_x);
    queue.pointer_y = (queue.pointer_y + delta_y).clamp(0, queue.pointer_max_y);
    let (x, y) = (queue.pointer_x, queue.pointer_y);
    push(InputEvent {
        kind: INPUT_MOUSE_MOVE,
        x,
        y,
        ..EMPTY
    });
}

/// Limita o ponteiro às dimensões da tela, para que ele não saia do visível.
pub fn clamp_pointer(width: i32, height: i32) {
    let queue = queue();
    queue.pointer_max_x = width.saturating_sub(1).max(0);
    queue.pointer_max_y = height.saturating_sub(1).max(0);
    queue.pointer_x = queue.pointer_x.clamp(0, queue.pointer_max_x);
    queue.pointer_y = queue.pointer_y.clamp(0, queue.pointer_max_y);
}

/// Registra um botão do ponteiro.
pub fn push_pointer_button(button: u32, pressed: bool) {
    let queue = queue();
    // Captura o estado atual antes de mutar: manter `&mut state` emprestado
    // enquanto lemos `pointer_x` conflitava com o borrow do mesmo `queue`.
    let was_pressed = match button {
        1 => queue.left_pressed,
        2 => queue.right_pressed,
        _ => return,
    };
    // Cada pacote PS/2 informa o estado completo dos botões. Só emite evento
    // quando há transição; emitir "pressed" em todo pacote fazia o WM começar
    // e cancelar o drag repetidamente, causando piscada e janelas saltando.
    if was_pressed == pressed {
        return;
    }
    match button {
        1 => queue.left_pressed = pressed,
        2 => queue.right_pressed = pressed,
        _ => {}
    }
    let (x, y) = (queue.pointer_x, queue.pointer_y);
    push(InputEvent {
        kind: INPUT_MOUSE_BUTTON,
        x,
        y,
        button,
        pressed: u32::from(pressed),
        ..EMPTY
    });
}

/// Retira o próximo evento, se houver.
pub fn poll() -> Option<InputEvent> {
    let queue = queue();
    if queue.length == 0 {
        return None;
    }
    let event = queue.events[queue.head];
    queue.head = (queue.head + 1) % CAPACITY;
    queue.length -= 1;
    Some(event)
}

/// Posição atual do ponteiro, para o servidor desenhar o cursor.
pub fn pointer() -> (i32, i32) {
    let queue = queue();
    (queue.pointer_x, queue.pointer_y)
}
