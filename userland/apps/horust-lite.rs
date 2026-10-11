//! horust-lite — supervisor de serviços em `no_std`, compatível com o formato
//! de serviços do Horust (`/etc/horust/services/*.toml`).
//!
//! Este é o andaime do port do Horust: enquanto o Horust original depende de
//! `std`/`nix` (veja `third_party/README.md`), esta versão implementa o mesmo
//! ciclo de vida de serviço sobre as syscalls POSIX que o ImagineOS já tem:
//!
//! - `fork` + `exec` para lançar cada serviço;
//! - `waitpid(WNOHANG)` para colher filhos sem bloquear (o papel do reaper);
//! - sinais SIGTERM/EXIT para o shutdown ordenado (`shutdown_after`).
//!
//! Não suportado ainda (falta de camada POSIX): threads de log por pipe,
//! `getpwnam` (campo `user`), healthiness por HTTP, resource limits.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use imagineos::abi::{ProcessInfo, SpawnIo, WNOHANG};
use imagineos::{console, fs, process, signals};

/// Caminho padrão das definições de serviço, como no Horust.
const SERVICES_DIR: &str = "/etc/horust/services";
const MAX_SERVICES: usize = 8;
const MAX_SERVICE_PATH: usize = 64;

/// Estado de um serviço supervisionado.
struct Service {
    /// Caminho do executável (campo `command` do TOML, primeiro token).
    command: [u8; MAX_SERVICE_PATH],
    command_length: usize,
    /// PID depois de lançado (0 = não lançado).
    pid: i64,
    /// Já coletou a saída do filho.
    exited: bool,
}

impl Service {
    const EMPTY: Self = Self {
        command: [0; MAX_SERVICE_PATH],
        command_length: 0,
        pid: 0,
        exited: false,
    };

    fn command(&self) -> &str {
        core::str::from_utf8(&self.command[..self.command_length]).unwrap_or("")
    }
}

#[unsafe(no_mangle)]
extern "C" fn _start(
    _argc: usize,
    _argv: *const *const u8,
    _envc: usize,
    _envp: *const *const u8,
) -> ! {
    // Ignora SIGINT/TERM no próprio supervisor; os filhos recebem os seus.
    let _ = signals::ignore(imagineos::abi::SIGNAL_INT);
    let _ = signals::ignore(imagineos::abi::SIGNAL_TERM);

    let mut services = [Service::EMPTY; MAX_SERVICES];
    let count = load_services(&mut services);

    if count == 0 {
        let _ = console::write_all(b"horust: no services in /etc/horust/services; exiting\n");
        process::exit(0);
    }
    let _ = console::write_all(b"horust: supervising services\n");

    // Lança todos os serviços (o Horust real respeita `start-after`; aqui os
    // serviços partem juntos, supervisão simples).
    for service in services.iter_mut().take(count) {
        launch(service);
    }

    // Laço do reaper: colhe filhos com WNOHANG, sem bloquear nenhum ciclo.
    loop {
        reap(&mut services, count);
        if services[..count].iter().all(|service| service.exited) {
            let _ = console::write_all(b"horust: all services exited; stopping\n");
            process::exit(0);
        }
        if process::yield_now().is_err() {
            process::exit(1);
        }
    }
}

/// Lê o diretório de serviços e extrai o comando de cada `*.toml`.
fn load_services(output: &mut [Service; MAX_SERVICES]) -> usize {
    let mut listing = [0u8; 2048];
    let Ok(used) = fs::list_directory(SERVICES_DIR, &mut listing) else {
        return 0;
    };
    let mut count = 0;
    let mut cursor = 0;
    while cursor < used && count < MAX_SERVICES {
        let end = listing[cursor..used]
            .iter()
            .position(|&byte| byte == 0 || byte == b'\n')
            .map(|offset| cursor + offset)
            .unwrap_or(used);
        let name = &listing[cursor..end];
        cursor = end + 1;
        if !name.ends_with(b".toml") {
            continue;
        }
        if let Some(service) = parse_service(SERVICES_DIR, name) {
            output[count] = service;
            count += 1;
        }
    }
    count
}

/// Faz o parse mínimo de um serviço: campo `command = "..."` do TOML.
/// Só o executável interessa nesta versão; flags são preservadas como
/// argumentos separados por espaço (split simples, sem aspas aninhadas).
fn parse_service(directory: &str, name: &[u8]) -> Option<Service> {
    let mut path = [0u8; 128];
    let directory_bytes = directory.as_bytes();
    if directory_bytes.len() + 1 + name.len() >= path.len() {
        return None;
    }
    let mut length = 0;
    path[..directory_bytes.len()].copy_from_slice(directory_bytes);
    length = directory_bytes.len();
    path[length] = b'/';
    length += 1;
    path[length..length + name.len()].copy_from_slice(name);
    length += name.len();
    let Some(file_path) = core::str::from_utf8(&path[..length]).ok() else {
        return None;
    };
    let mut contents = [0u8; 1024];
    let Ok(used) = fs::read_file(file_path, &mut contents) else {
        return None;
    };
    let contents = &contents[..used];
    // Procura `command = "` e lê até o fecha-aspa.
    let needle = b"command";
    let mut index = 0;
    while index + needle.len() < contents.len() {
        if &contents[index..index + needle.len()] == needle {
            // Pula para o `= "`.
            let mut cursor = index + needle.len();
            while cursor < contents.len() && (contents[cursor] == b' ' || contents[cursor] == b'\t')
            {
                cursor += 1;
            }
            if cursor < contents.len() && contents[cursor] == b'=' {
                cursor += 1;
                while cursor < contents.len()
                    && (contents[cursor] == b' ' || contents[cursor] == b'\t')
                {
                    cursor += 1;
                }
                if cursor < contents.len() && contents[cursor] == b'"' {
                    cursor += 1;
                    let start = cursor;
                    while cursor < contents.len()
                        && contents[cursor] != b'"'
                        && contents[cursor] != b'\n'
                    {
                        cursor += 1;
                    }
                    let value = &contents[start..cursor];
                    return Some(Service {
                        command: command_bytes(value),
                        command_length: value.len(),
                        pid: 0,
                        exited: false,
                    });
                }
            }
        }
        index += 1;
    }
    None
}

fn command_bytes(value: &[u8]) -> [u8; MAX_SERVICE_PATH] {
    let mut command = [0u8; MAX_SERVICE_PATH];
    let length = value.len().min(MAX_SERVICE_PATH);
    command[..length].copy_from_slice(&value[..length]);
    command
}

/// Lança o serviço com fork + exec (padrão POSIX de um supervisor).
fn launch(service: &mut Service) {
    // Copia o comando para fora do borrow antes de mutar o PID.
    let mut command = [0u8; MAX_SERVICE_PATH];
    let command_length = service.command_length;
    command[..command_length].copy_from_slice(&service.command[..command_length]);
    let command = core::str::from_utf8(&command[..command_length]).unwrap_or("");
    let mut tokens = command.split(' ').filter(|token| !token.is_empty());
    let Some(path) = tokens.next() else {
        return;
    };
    let mut arguments = [""; 8];
    let mut argument_count = 0;
    for token in tokens {
        if argument_count == arguments.len() {
            break;
        }
        arguments[argument_count] = token;
        argument_count += 1;
    }
    match process::spawn(path, &arguments[..argument_count], &[], imagineos::abi::SpawnIo::default()) {
        Ok(pid) => {
            service.pid = pid as i64;
            let _ = console::write_all(b"horust: started ");
            let _ = console::write_all(path.as_bytes());
            let _ = console::write_all(b" (pid ");
            // PID decimal mínimo, sem fmt float.
            write_number(service.pid as u64);
            let _ = console::write_all(b")\n");
        }
        Err(_) => {
            let _ = console::write_all(b"horust: failed to start ");
            let _ = console::write_all(path.as_bytes());
            let _ = console::write_all(b"\n");
            service.exited = true;
        }
    }
}

/// Colhe filhos terminados (o papel do `reaper` do Horust original).
fn reap(services: &mut [Service; MAX_SERVICES], count: usize) {
    // -1 = qualquer filho, WNOHANG = não bloqueia (POSIX).
    while let Ok(Some((pid, _status))) = process::waitpid_nonblocking(-1) {
        for service in services.iter_mut().take(count) {
            if service.pid == pid as i64 {
                service.exited = true;
                let _ = console::write_all(b"horust: service exited (pid ");
                write_number(pid as u64);
                let _ = console::write_all(b")\n");
            }
        }
    }
}

fn write_number(mut value: u64) {
    let mut buffer = [0u8; 20];
    let mut length = 0;
    if value == 0 {
        buffer[0] = b'0';
        length = 1;
    }
    while value > 0 {
        buffer[length] = b'0' + (value % 10) as u8;
        length += 1;
        value /= 10;
    }
    for index in 0..length / 2 {
        buffer.swap(index, length - 1 - index);
    }
    let _ = console::write_all(&buffer[..length]);
}

/// Unused: mantém o tipo importado para futuras listagens de serviços vivos.
#[allow(dead_code)]
fn _touch(_info: ProcessInfo) {}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process::exit(101)
}
