use crate::{args, console, fs, process};

pub unsafe fn argument(argv: *const *const u8, index: usize) -> Option<&'static [u8]> {
    args::argument(argv, index.saturating_add(1), index)
}

pub unsafe fn environment_value(
    envp: *const *const u8,
    count: usize,
    name: &[u8],
) -> Option<&'static [u8]> {
    args::environment_value(envp, count, name)
}

pub fn resolve_path(
    envp: *const *const u8,
    envc: usize,
    path: &[u8],
    output: &mut [u8],
) -> Option<usize> {
    let cwd = unsafe { environment_value(envp, envc, b"PWD") };
    args::resolve_path(cwd, path, output)
}

pub fn write(bytes: &[u8]) {
    if console::write_all(bytes).is_err() {
        process::exit(1);
    }
}

pub fn read_char() -> char {
    match console::read_char() {
        Ok(character) => character,
        Err(_) => process::exit(1),
    }
}

pub fn clear() {
    if console::clear().is_err() {
        process::exit(1);
    }
}

pub fn read_file(path: &[u8], output: &mut [u8]) -> i64 {
    let Ok(path) = core::str::from_utf8(path) else {
        return -22;
    };
    fs::read_file(path, output).map_or_else(|error| -(error.code() as i64), |count| count as i64)
}

pub fn write_file(path: &[u8], contents: &[u8]) -> i64 {
    let Ok(path) = core::str::from_utf8(path) else {
        return -22;
    };
    fs::write_file(path, contents).map_or_else(|error| -(error.code() as i64), |count| count as i64)
}

pub fn list_directory(path: &[u8], output: &mut [u8]) -> i64 {
    let Ok(path) = core::str::from_utf8(path) else {
        return -22;
    };
    fs::list_directory(path, output)
        .map_or_else(|error| -(error.code() as i64), |count| count as i64)
}

pub fn mkdir(path: &[u8], parents: bool) -> i64 {
    let Ok(path) = core::str::from_utf8(path) else {
        return -22;
    };
    fs::mkdir(path, parents).map_or_else(|error| -(error.code() as i64), |_| 0)
}

pub fn touch(path: &[u8]) -> i64 {
    let Ok(path) = core::str::from_utf8(path) else {
        return -22;
    };
    fs::touch(path).map_or_else(|error| -(error.code() as i64), |_| 0)
}

pub fn remove(path: &[u8], recursive: bool) -> i64 {
    let Ok(path) = core::str::from_utf8(path) else {
        return -22;
    };
    fs::remove(path, recursive).map_or_else(|error| -(error.code() as i64), |_| 0)
}

/// Lê de um descritor até ele chegar ao fim ou o buffer encher.
///
/// Devolve quantos bytes foram lidos, ou um código negativo de erro. Para um
/// pipeline (`cmd | filtro`), o `fd` é uma ponta de pipe: `read` devolve 0 ao
/// ver EOF e `EAGAIN` (`-11`) enquanto o produtor ainda não escreveu. Aqui
/// `EAGAIN` é tratado cedendo a CPU e tentando de novo, para que o filtro
/// funcione igual a um programa POSIX mesmo sem bloqueio real no kernel.
pub fn read_fd(fd: usize, output: &mut [u8]) -> i64 {
    let mut total = 0usize;
    while total < output.len() {
        match fs::read(fd, &mut output[total..]) {
            Ok(0) => break,
            Ok(count) => total += count,
            Err(error) if error.code() == 11 => {
                // EAGAIN: o escritor ainda não produziu. Cede e tenta de novo.
                if process::yield_now().is_err() {
                    break;
                }
            }
            Err(_) if total != 0 => break,
            Err(error) => return -(error.code() as i64),
        }
    }
    total as i64
}

/// Lê todo o stdin do processo, útil para filtros de pipeline.
pub fn read_stdin(output: &mut [u8]) -> i64 {
    read_fd(fs::stdin(), output)
}

pub fn exit(status: u64) -> ! {
    process::exit(status as i32)
}
