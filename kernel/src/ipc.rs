//! Pipes de kernel para pipelines de shell.
//!
//! Um pipe é um buffer circular de `PIPE_CAPACITY` bytes identificado por um
//! índice (`PipeId`). As duas pontas são descritores comuns: o lado de leitura
//! consome bytes, o de escrita produz. Os descritores guardam apenas o id do
//! pipe mais a direção (ver [`crate::process::DescriptorKind`]), então `dup2`
//! funciona de forma idêntica a um arquivo.
//!
//! Não há bloqueio real: o escalonador é cooperativo e o kernel roda com a
//! pilha do processo. Quando um lado do pipe está cheio ou vazio, a syscall
//! devolve `-EAGAIN`. O shell trata isso com `yield` e nova tentativa, o que é
//! suficiente para pipelines e mantém o kernel sem filas de espera.

use core::cell::UnsafeCell;

use imagineos_abi::{PIPE_CAPACITY, MAX_PIPES};

/// Índice de um pipe na tabela global.
pub type PipeId = usize;

/// Erros que um pipe pode devolver. Convertidos para errno pelo dispatcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipeError {
    /// Não há slots de pipe livres.
    TooMany,
    /// O id não corresponde a um pipe aberto.
    Invalid,
    /// O buffer está cheio (escrita) ou vazio (leitura) — `EAGAIN`.
    WouldBlock,
    /// A ponta oposta já foi fechada por todos os donos.
    Broken,
}

impl PipeError {
    pub fn to_errno(self) -> i64 {
        match self {
            PipeError::TooMany => -23,   // ENFILE
            PipeError::Invalid => -9,    // EBADF
            PipeError::WouldBlock => -11, // EAGAIN
            PipeError::Broken => -32,    // EPIPE
        }
    }
}

#[derive(Clone, Copy)]
struct Pipe {
    open: bool,
    /// Número de descritores de leitura vivos (contagem de referências lógicas).
    readers: usize,
    /// Número de descritores de escrita vivos.
    writers: usize,
    /// Posição do próximo byte a ler.
    head: usize,
    /// Posição do próximo byte a escrever.
    tail: usize,
    /// Quantidade de bytes atualmente no buffer.
    length: usize,
    buffer: [u8; PIPE_CAPACITY],
}

impl Pipe {
    const EMPTY: Self = Self {
        open: false,
        readers: 0,
        writers: 0,
        head: 0,
        tail: 0,
        length: 0,
        buffer: [0; PIPE_CAPACITY],
    };

    const fn new() -> Self {
        Self::EMPTY
    }
}

struct SharedPipes(UnsafeCell<[Pipe; MAX_PIPES]>);
unsafe impl Sync for SharedPipes {}

static PIPES: SharedPipes = SharedPipes(UnsafeCell::new([Pipe::EMPTY; MAX_PIPES]));

/// Cria um pipe vazio e devolve o seu id. O id fica com um leitor e um
/// escritor; `open_read`/`open_write` incrementam essas contagens.
pub fn create() -> Result<PipeId, PipeError> {
    let pipes = unsafe { &mut *PIPES.0.get() };
    let Some(id) = pipes.iter().position(|pipe| !pipe.open) else {
        return Err(PipeError::TooMany);
    };
    pipes[id] = Pipe::new();
    pipes[id].open = true;
    pipes[id].readers = 1;
    pipes[id].writers = 1;
    Ok(id)
}

fn pipe(id: PipeId) -> Result<&'static mut Pipe, PipeError> {
    let pipes = unsafe { &mut *PIPES.0.get() };
    let Some(pipe) = pipes.get_mut(id) else {
        return Err(PipeError::Invalid);
    };
    if !pipe.open {
        return Err(PipeError::Invalid);
    }
    Ok(pipe)
}

/// Registra um novo descritor de leitura para `id`.
pub fn add_reader(id: PipeId) -> Result<(), PipeError> {
    let pipe = pipe(id)?;
    pipe.readers = pipe.readers.saturating_add(1);
    Ok(())
}

/// Registra um novo descritor de escrita para `id`.
pub fn add_writer(id: PipeId) -> Result<(), PipeError> {
    let pipe = pipe(id)?;
    pipe.writers = pipe.writers.saturating_add(1);
    Ok(())
}

/// Libera um descritor de leitura. O pipe é reciclado quando ambos os lados
/// chegam a zero, o que descarta qualquer dado restante (comportamento
/// esperado quando o shell fecha a ponta de leitura de um pipe quebrado).
pub fn close_reader(id: PipeId) {
    if let Ok(pipe) = pipe(id) {
        pipe.readers = pipe.readers.saturating_sub(1);
        if pipe.readers == 0 && pipe.writers == 0 {
            *pipe = Pipe::EMPTY;
        }
    }
}

/// Libera um descritor de escrita.
pub fn close_writer(id: PipeId) {
    if let Ok(pipe) = pipe(id) {
        pipe.writers = pipe.writers.saturating_sub(1);
        if pipe.readers == 0 && pipe.writers == 0 {
            *pipe = Pipe::EMPTY;
        }
    }
}

/// Lê até `output.len()` bytes do pipe.
///
/// Devolve `0` quando a ponta de escrita já fechou *e* não há dados (EOF real,
/// como em `read(2)`), e `WouldBlock` quando ainda há escritores mas o buffer
/// está temporariamente vazio.
pub fn read(id: PipeId, output: &mut [u8]) -> Result<usize, PipeError> {
    let pipe = pipe(id)?;
    if pipe.length == 0 {
        return if pipe.writers == 0 {
            Ok(0)
        } else {
            Err(PipeError::WouldBlock)
        };
    }
    let length = output.len().min(pipe.length);
    for offset in 0..length {
        output[offset] = pipe.buffer[pipe.head];
        pipe.head = (pipe.head + 1) % PIPE_CAPACITY;
    }
    pipe.length -= length;
    Ok(length)
}

/// Escreve o máximo possível de `input` no pipe.
///
/// Devolve quantos bytes entraram. Se a ponta de leitura já fechou, devolve
/// `Broken` para que o escritor receba `EPIPE`.
pub fn write(id: PipeId, input: &[u8]) -> Result<usize, PipeError> {
    let pipe = pipe(id)?;
    if pipe.readers == 0 {
        return Err(PipeError::Broken);
    }
    if input.is_empty() {
        return Ok(0);
    }
    if pipe.length == PIPE_CAPACITY {
        return Err(PipeError::WouldBlock);
    }
    let space = PIPE_CAPACITY - pipe.length;
    let length = input.len().min(space);
    for offset in 0..length {
        pipe.buffer[pipe.tail] = input[offset];
        pipe.tail = (pipe.tail + 1) % PIPE_CAPACITY;
    }
    pipe.length += length;
    Ok(length)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_then_reads_in_order() {
        let id = create().expect("pipe");
        assert_eq!(write(id, b"hello"), Ok(5));
        let mut buffer = [0u8; 5];
        assert_eq!(read(id, &mut buffer), Ok(5));
        assert_eq!(&buffer, b"hello");
        close_reader(id);
        close_writer(id);
    }

    #[test]
    fn read_reports_would_block_then_eof() {
        let id = create().expect("pipe");
        let mut buffer = [0u8; 4];
        // Still has a writer, nothing buffered: transient.
        assert_eq!(read(id, &mut buffer), Err(PipeError::WouldBlock));
        close_writer(id);
        // Writer gone and empty: EOF.
        assert_eq!(read(id, &mut buffer), Ok(0));
        close_reader(id);
    }

    #[test]
    fn write_to_closed_reader_is_broken() {
        let id = create().expect("pipe");
        close_reader(id);
        assert_eq!(write(id, b"x"), Err(PipeError::Broken));
        close_writer(id);
    }

    #[test]
    fn write_stops_when_the_ring_is_full() {
        let id = create().expect("pipe");
        let chunk = [7u8; 512];
        let mut total = 0usize;
        loop {
            match write(id, &chunk) {
                Ok(0) => break,
                Ok(n) => total += n,
                Err(PipeError::WouldBlock) => break,
                Err(other) => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(total, PIPE_CAPACITY);
        // Draining the ring frees space again.
        let mut drain = [0u8; 512];
        assert_eq!(read(id, &mut drain), Ok(512));
        assert_eq!(write(id, &chunk), Ok(512));
        close_reader(id);
        close_writer(id);
    }
}
