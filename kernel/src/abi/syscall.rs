use core::arch::asm;
use core::cell::UnsafeCell;

use crate::keyboard::Keyboard;
use crate::process::UserArg;
use crate::{framebuffer, process};
use imagineos_abi::UserStat;
use imagineos_abi::SYS_STAT;
use imagineos_abi::{
    OPEN_APPEND, OPEN_CREATE, OPEN_EXCLUSIVE, OPEN_READ, OPEN_TRUNCATE, OPEN_WRITE,
    SYS_ABI_VERSION,
    SYS_AUTHENTICATE, SYS_CLEAR, SYS_CLOSE, SYS_DISK_COUNT, SYS_DISK_SECTORS, SYS_DMESG, SYS_EXEC,
    SYS_EXIT, SYS_GETIDENTITY, SYS_GETPID, SYS_INSTALL_DISK, SYS_INSTALL_DISK_CONFIG, SYS_ISDIR,
    SYS_ISFILE, SYS_MKDIR, SYS_OPEN, SYS_POWER_OFF, SYS_READ, SYS_READDIR, SYS_READ_FD,
    SYS_READ_FILE, SYS_REMOVE, SYS_SHUTDOWN_POLL, SYS_SHUTDOWN_REQUEST, SYS_TOUCH, SYS_WRITE,
    SYS_FSTAT, SYS_GETTIMEOFDAY, SYS_SBRK, SYS_LSEEK, SYS_WRITE_FD, SYS_WRITE_FILE, SYS_YIELD,
    SYS_DUP, SYS_DUP2, SYS_PIPE, SYS_SPAWN, SYS_FORK, SYS_WAITPID, SYS_EXECVE,
    SYS_FB_INFO, SYS_FB_MAP, SYS_INPUT_POLL, SYS_CONSOLE_RELEASE, SYS_POINTER_POSITION,
    SYS_SURFACE_CREATE, SYS_SURFACE_PRESENT, SYS_SURFACE_FLUSH, SYS_SURFACE_DESTROY,
    SYS_SURFACE_POLL, SYS_SURFACE_SNAPSHOT, SYS_SURFACE_SEND_EVENT, SYS_PIVOT_ROOT, WNOHANG,
};

struct SharedKeyboard(UnsafeCell<Keyboard>);
unsafe impl Sync for SharedKeyboard {}
static KEYBOARD: SharedKeyboard = SharedKeyboard(UnsafeCell::new(Keyboard::new()));

struct SharedUtf8Decoder(UnsafeCell<crate::utf8::Decoder>);
unsafe impl Sync for SharedUtf8Decoder {}
static SERIAL_UTF8: SharedUtf8Decoder =
    SharedUtf8Decoder(UnsafeCell::new(crate::utf8::Decoder::new()));

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

#[no_mangle]
extern "C" fn dreamcore_syscall_dispatch(frame: *mut TrapFrame) -> *mut TrapFrame {
    // Preserva os registradores FPU/SSE do processo que fez a syscall: o kernel
    // Rust pode usar XMM entre aqui e o retorno para o espaço de usuário.
    process::save_current_fpu();
    let next = syscall_dispatch_inner(frame);
    let next = process::deliver_pending_signal(next);
    process::restore_fpu_for(next);
    next
}

fn syscall_dispatch_inner(frame: *mut TrapFrame) -> *mut TrapFrame {
    let Some(frame_ref) = (unsafe { frame.as_mut() }) else {
        return frame;
    };
    match frame_ref.rax {
        SYS_YIELD => {
            frame_ref.rax = 0;
            process::yield_current(frame)
        }
        SYS_EXIT => {
            // `rdi` traz o status de saída, como `exit(status)` no POSIX.
            // O pai o recebe em `waitpid` e em `$?`.
            process::exit_current(frame, frame_ref.rdi as i32)
        }
        SYS_GETPID => {
            frame_ref.rax = process::current_pid() as u64;
            frame
        }
        SYS_GETIDENTITY => {
            let identity = process::current_identity();
            let bytes = unsafe {
                core::slice::from_raw_parts(
                    (&identity as *const imagineos_abi::UserIdentity).cast::<u8>(),
                    core::mem::size_of::<imagineos_abi::UserIdentity>(),
                )
            };
            frame_ref.rax = if process::copy_to_current_user(frame_ref.rdi, bytes) {
                0
            } else {
                (-14i64) as u64
            };
            frame
        }
        SYS_AUTHENTICATE => {
            let username_length = frame_ref.rsi as usize;
            let password_length = frame_ref.r10 as usize;
            let mut username = [0u8; imagineos_abi::ACCOUNT_NAME_SIZE];
            let mut password = [0u8; imagineos_abi::ACCOUNT_PASSWORD_SIZE];
            if username_length == 0
                || username_length > username.len()
                || password_length == 0
                || password_length > password.len()
                || !process::copy_from_current_user(frame_ref.rdi, &mut username[..username_length])
                || !process::copy_from_current_user(frame_ref.rdx, &mut password[..password_length])
                || username[..username_length].contains(&0)
                || password[..password_length].contains(&0)
            {
                frame_ref.rax = (-22i64) as u64;
            } else {
                frame_ref.rax = process::authenticate_current(
                    &username[..username_length],
                    &password[..password_length],
                    frame_ref.r8 as u32,
                ) as u64;
            }
            password.fill(0);
            frame
        }
        SYS_ABI_VERSION => {
            frame_ref.rax = imagineos_abi::ABI_VERSION;
            frame
        }
        SYS_SBRK => {
            frame_ref.rax = process::grow_heap(frame_ref.rdi as i64)
                .map_or_else(|error| error as u64, |address| address);
            frame
        }
        SYS_LSEEK => seek_fd(frame_ref),
        SYS_FSTAT => fstat_fd(frame_ref),
        SYS_GETTIMEOFDAY => gettimeofday(frame_ref),
        SYS_SHUTDOWN_REQUEST => {
            frame_ref.rax = if process::current_credentials().0 == 0 {
                crate::power::request_shutdown();
                0
            } else {
                (-1i64) as u64
            };
            frame
        }
        SYS_SHUTDOWN_POLL => {
            frame_ref.rax = if process::current_pid() != 1 {
                (-1i64) as u64
            } else {
                u64::from(crate::power::take_shutdown_request())
            };
            frame
        }
        SYS_POWER_OFF => {
            if process::current_pid() != 1 {
                frame_ref.rax = (-1i64) as u64;
                frame
            } else {
                match crate::power::power_off() {
                    Ok(()) => unreachable!(),
                    Err(_) => {
                        crate::console_write(
                            "ERROR: storage sync or ACPI poweroff failed; shutdown aborted\n",
                        );
                        frame_ref.rax = (-5i64) as u64;
                        frame
                    }
                }
            }
        }
        imagineos_abi::SYS_KILL => {
            frame_ref.rax =
                process::send_signal(frame_ref.rdi as usize, frame_ref.rsi as u64) as u64;
            frame
        }
        imagineos_abi::SYS_SIGACTION => {
            frame_ref.rax = process::set_signal_action(
                frame_ref.rdi as u64,
                frame_ref.rsi as u64,
                frame_ref.rdx as u64,
            ) as u64;
            frame
        }
        imagineos_abi::SYS_SIGRETURN => {
            if !process::restore_signal_context(frame) {
                frame_ref.rax = (-22i64) as u64;
            }
            frame
        }
        imagineos_abi::SYS_PROCESS_LIST => {
            let capacity = frame_ref.rsi as usize;
            if capacity > imagineos_abi::MAX_PROCESSES {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            }
            let (processes, count) = process::snapshot();
            let length = count.min(capacity);
            let bytes = unsafe {
                core::slice::from_raw_parts(
                    processes.as_ptr().cast::<u8>(),
                    length * core::mem::size_of::<imagineos_abi::ProcessInfo>(),
                )
            };
            frame_ref.rax = if process::copy_to_current_user(frame_ref.rdi, bytes) {
                length as u64
            } else {
                (-14i64) as u64
            };
            frame
        }
        SYS_DMESG => {
            let capacity = frame_ref.rsi as usize;
            if capacity > imagineos_abi::MAX_READ_BUFFER {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            }
            let mut buffer = [0u8; imagineos_abi::MAX_READ_BUFFER];
            let length = crate::kernel_log::read(&mut buffer[..capacity]);
            frame_ref.rax = if process::copy_to_current_user(frame_ref.rdi, &buffer[..length]) {
                length as u64
            } else {
                (-14i64) as u64
            };
            frame
        }
        SYS_DISK_COUNT => {
            frame_ref.rax = u64::from(crate::ata::is_ready());
            frame
        }
        SYS_DISK_SECTORS => {
            frame_ref.rax = if frame_ref.rdi == 0 && crate::ata::is_ready() {
                crate::ata::sector_count()
            } else {
                (-19i64) as u64
            };
            frame
        }
        SYS_INSTALL_DISK | SYS_INSTALL_DISK_CONFIG => {
            frame_ref.rax = if !process::can_install_to_disk()
                || process::current_credentials().0 != 0
            {
                (-1i64) as u64
            } else if frame_ref.rdi != 0 {
                (-19i64) as u64
            } else {
                let mut install_config = imagineos_abi::InstallConfig::default();
                let config_is_valid = frame_ref.rax == SYS_INSTALL_DISK
                    || process::copy_from_current_user(frame_ref.rsi, unsafe {
                        core::slice::from_raw_parts_mut(
                            (&mut install_config as *mut imagineos_abi::InstallConfig).cast::<u8>(),
                            core::mem::size_of::<imagineos_abi::InstallConfig>(),
                        )
                    });
                if !config_is_valid {
                    frame_ref.rax = (-14i64) as u64;
                    return frame;
                }
                if let Err(()) = crate::accounts::validate_install_config(&install_config) {
                    frame_ref.rax = (-22i64) as u64;
                    return frame;
                }
                let result = crate::installer::install_primary_master(&install_config);
                install_config.password.fill(0);
                match result {
                    Ok(()) => 0,
                    Err(crate::installer::InstallError::NoDisk) => (-19i64) as u64,
                    Err(crate::installer::InstallError::DiskTooSmall) => (-28i64) as u64,
                    Err(crate::installer::InstallError::MissingPayload) => (-2i64) as u64,
                    Err(crate::installer::InstallError::PayloadTooLarge) => (-28i64) as u64,
                    Err(crate::installer::InstallError::DfsFormat) => (-5i64) as u64,
                    Err(crate::installer::InstallError::InvalidAccountConfig) => (-22i64) as u64,
                    Err(crate::installer::InstallError::EntropyUnavailable) => (-95i64) as u64,
                    Err(crate::installer::InstallError::Block(_)) => (-5i64) as u64,
                }
            };
            frame
        }
        SYS_WRITE => {
            let length = (frame_ref.rsi as usize).min(imagineos_abi::MAX_CONSOLE_WRITE);
            let mut buffer = [0u8; 512];
            if process::copy_from_current_user(frame_ref.rdi, &mut buffer[..length]) {
                crate::console_write_bytes(&buffer[..length]);
                frame_ref.rax = length as u64;
            } else {
                frame_ref.rax = (-14i64) as u64;
            }
            frame
        }
        SYS_READ => {
            frame_ref.rax = read_character() as u64;
            frame
        }
        SYS_CLEAR => {
            framebuffer::clear();
            frame_ref.rax = 0;
            frame
        }
        SYS_FB_INFO => framebuffer_info(frame_ref),
        SYS_FB_MAP => {
            let result = framebuffer_map(frame_ref);
            // Restringe o ponteiro à área visível: o servidor de display pode
            // mapear a tela depois de alguns movimentos do mouse já terem sido
            // acumulados, e um cursor fora da tela seria desenhado no vazio.
            if let Some(description) = crate::framebuffer::description() {
                crate::input::clamp_pointer(description.width as i32, description.height as i32);
            }
            result
        }
        SYS_INPUT_POLL => input_poll(frame_ref),
        SYS_POINTER_POSITION => {
            // O servidor de display desenha o cursor na posição atual sem
            // precisar rastrear cada movimento acumulado desde o último poll.
            let (x, y) = crate::input::pointer();
            frame_ref.rax = ((x as u32 as u64) << 32) | (y as u32 as u64);
            frame
        }
        SYS_CONSOLE_RELEASE => {
            // O servidor de display assume a tela: o console do kernel para de
            // desenhar nela e os eventos passam a ser entregues pela fila.
            crate::framebuffer::set_active(false);
            crate::input::capture();
            crate::surface::set_server();
            frame_ref.rax = 0;
            frame
        }
        SYS_SURFACE_CREATE => surface_create(frame_ref),
        SYS_SURFACE_PRESENT => surface_present(frame_ref),
        SYS_SURFACE_FLUSH => surface_flush(frame_ref),
        SYS_SURFACE_DESTROY => surface_destroy(frame_ref),
        SYS_SURFACE_POLL => surface_poll(frame_ref),
        SYS_SURFACE_SNAPSHOT => surface_snapshot(frame_ref),
        SYS_SURFACE_SEND_EVENT => surface_send_event(frame_ref),
        SYS_EXEC | SYS_EXECVE => {
            // `exec` cria outro processo (histórico); `execve` substitui a imagem
            // do processo atual, completando o par `fork` + `exec` do POSIX.
            let replace = frame_ref.rax == SYS_EXECVE;
            let mut decoded = ExecArguments::new();
            match decoded.load(frame_ref) {
                Ok(()) => {}
                Err(error) => {
                    frame_ref.rax = error as u64;
                    return frame;
                }
            }
            // As fatias devolvidas têm o tamanho máximo do vetor; usar o vetor
            // inteiro faria `argc` chegar como `MAX_EXEC_ARGS` ao processo.
            let arguments = decoded.arguments();
            let arguments = &arguments[..decoded.argument_count()];
            let environment = decoded.environment();
            let environment = &environment[..decoded.environment_count()];
            if replace {
                process::execve_current(decoded.path(), arguments, environment, frame)
            } else {
                process::spawn_current(decoded.path(), arguments, environment, frame)
            }
        }
        SYS_ISDIR => {
            let length = frame_ref.rsi as usize;
            if length == 0 || length > imagineos_abi::MAX_PATH_QUERY {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            }
            let mut path = [0u8; 128];
            if !process::copy_from_current_user(frame_ref.rdi, &mut path[..length]) {
                frame_ref.rax = (-14i64) as u64;
                return frame;
            }
            if path[..length].contains(&0) {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            }
            let Ok(path) = core::str::from_utf8(&path[..length]) else {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            };
            if !access_allowed(path, 4) {
                frame_ref.rax = (-13i64) as u64;
                return frame;
            }
            frame_ref.rax = crate::ramfs::is_directory(path) as u64;
            frame
        }
        SYS_ISFILE => {
            let length = frame_ref.rsi as usize;
            if length == 0 || length > imagineos_abi::MAX_PATH_QUERY {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            }
            let mut path = [0u8; 128];
            if !process::copy_from_current_user(frame_ref.rdi, &mut path[..length]) {
                frame_ref.rax = (-14i64) as u64;
                return frame;
            }
            if path[..length].contains(&0) {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            }
            let Ok(path) = core::str::from_utf8(&path[..length]) else {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            };
            if !access_allowed(path, 4) {
                frame_ref.rax = (-13i64) as u64;
                return frame;
            }
            frame_ref.rax = crate::ramfs::is_file(path) as u64;
            frame
        }
        SYS_READ_FILE | SYS_READDIR => {
            let path_length = frame_ref.rsi as usize;
            let capacity = frame_ref.r10 as usize;
            if path_length == 0
                || path_length > imagineos_abi::MAX_PATH_QUERY
                || capacity > imagineos_abi::MAX_READ_BUFFER
            {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            }
            let mut path = [0u8; 128];
            if !process::copy_from_current_user(frame_ref.rdi, &mut path[..path_length]) {
                frame_ref.rax = (-14i64) as u64;
                return frame;
            }
            if path[..path_length].contains(&0) {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            }
            let Ok(path) = core::str::from_utf8(&path[..path_length]) else {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            };
            if !access_allowed(path, if frame_ref.rax == SYS_READ_FILE { 4 } else { 5 }) {
                frame_ref.rax = (-13i64) as u64;
                return frame;
            }
            let mut buffer = [0u8; imagineos_abi::MAX_READ_BUFFER];
            let length = if frame_ref.rax == SYS_READ_FILE {
                if crate::ramfs::is_directory(path) {
                    frame_ref.rax = (-21i64) as u64;
                    return frame;
                }
                let Some(bytes) = crate::ramfs::read(path) else {
                    frame_ref.rax = (-2i64) as u64;
                    return frame;
                };
                if bytes.len() > capacity {
                    frame_ref.rax = (-75i64) as u64;
                    return frame;
                }
                buffer[..bytes.len()].copy_from_slice(bytes);
                bytes.len()
            } else {
                if !crate::ramfs::is_directory(path) {
                    frame_ref.rax = if crate::ramfs::is_file(path) {
                        (-20i64) as u64
                    } else {
                        (-2i64) as u64
                    };
                    return frame;
                }
                let Some(length) = crate::ramfs::list_directory_with_hidden(
                    path,
                    frame_ref.r9 != 0,
                    &mut buffer[..capacity],
                ) else {
                    frame_ref.rax = (-75i64) as u64;
                    return frame;
                };
                length
            };
            if !process::copy_to_current_user(frame_ref.rdx, &buffer[..length]) {
                frame_ref.rax = (-14i64) as u64;
            } else {
                frame_ref.rax = length as u64;
            }
            frame
        }
        SYS_STAT => {
            let path_length = frame_ref.rsi as usize;
            if path_length == 0 || path_length > imagineos_abi::MAX_PATH_QUERY {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            }
            let mut path = [0u8; 128];
            if !process::copy_from_current_user(frame_ref.rdi, &mut path[..path_length]) {
                frame_ref.rax = (-14i64) as u64;
                return frame;
            }
            if path[..path_length].contains(&0) {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            }
            let Ok(path) = core::str::from_utf8(&path[..path_length]) else {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            };
            let Some(metadata) = crate::ramfs::metadata(path) else {
                frame_ref.rax = (-2i64) as u64;
                return frame;
            };
            let result = UserStat {
                size: metadata.size,
                mode: metadata.mode as u32,
                uid: metadata.uid,
                gid: metadata.gid,
                kind: u32::from(metadata.is_directory),
            };
            let bytes = unsafe {
                core::slice::from_raw_parts(
                    (&result as *const UserStat).cast::<u8>(),
                    core::mem::size_of::<UserStat>(),
                )
            };
            frame_ref.rax = if process::copy_to_current_user(frame_ref.rdx, bytes) {
                0
            } else {
                (-14i64) as u64
            };
            frame
        }
        SYS_WRITE_FILE => {
            let mut path_buffer = [0u8; 256];
            let path = match copy_user_path(frame_ref.rdi, frame_ref.rsi, &mut path_buffer) {
                Ok(path) => path,
                Err(error) => {
                    frame_ref.rax = error as u64;
                    return frame;
                }
            };
            let length = match usize::try_from(frame_ref.r10) {
                Ok(length) if length <= crate::ramfs::MAX_WRITE_FILE_SIZE => length,
                _ => {
                    frame_ref.rax = (-28i64) as u64;
                    return frame;
                }
            };
            let allowed = if crate::ramfs::is_file(path) {
                access_allowed(path, 2)
            } else {
                parent_access_allowed(path, 3)
            };
            if !allowed {
                frame_ref.rax = (-13i64) as u64;
                return frame;
            }
            let mut contents = [0u8; imagineos_abi::MAX_WRITE_FILE_SIZE];
            if !process::copy_from_current_user(frame_ref.rdx, &mut contents[..length]) {
                frame_ref.rax = (-14i64) as u64;
                return frame;
            }
            if crate::config::is_global_config_path(path)
                && crate::config::validate(&contents[..length]).is_err()
            {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            }
            let (uid, gid, _) = process::current_credentials();
            match crate::ramfs::write_file_as(path, &contents[..length], uid, gid) {
                Ok(()) => {
                    if crate::config::is_global_config_path(path) {
                        let _ = crate::config::load(&contents[..length]);
                    }
                    frame_ref.rax = length as u64;
                }
                Err(error) => frame_ref.rax = fs_error_code(error) as u64,
            }
            frame
        }
        SYS_OPEN => open_file(frame_ref),
        SYS_READ_FD => read_fd(frame_ref),
        SYS_WRITE_FD => write_fd(frame_ref),
        SYS_DUP => {
            frame_ref.rax = process::duplicate_descriptor(frame_ref.rdi as usize)
                .map_or_else(|error| error as u64, |fd| fd as u64);
            frame
        }
        SYS_DUP2 => {
            frame_ref.rax = process::duplicate_descriptor_to(
                frame_ref.rdi as usize,
                frame_ref.rsi as usize,
            )
            .map_or_else(|error| error as u64, |fd| fd as u64);
            frame
        }
        SYS_PIPE => create_pipe(frame_ref),
        SYS_SPAWN => spawn_with_io(frame_ref),
        SYS_FORK => process::fork_current(frame),
        SYS_WAITPID => process::waitpid_current(
            frame,
            frame_ref.rdi as i64,
            frame_ref.rsi,
            frame_ref.rdx as i64,
        ),
        SYS_PIVOT_ROOT => {
            // pivot_root(new_root, put_old): troca a raiz global. Os caminhos
            // vêm como bytes de usuário terminados em NUL, lidos para o kernel.
            let mut new_root = [0u8; imagineos_abi::MAX_MUTABLE_PATH];
            let mut put_old = [0u8; imagineos_abi::MAX_MUTABLE_PATH];
            let read_new = process::copy_from_current_user(frame_ref.rdi, &mut new_root)
                .then(|| {
                    let end = new_root.iter().position(|&byte| byte == 0).unwrap_or(0);
                    &new_root[..end]
                });
            let read_old = process::copy_from_current_user(frame_ref.rsi, &mut put_old).then(|| {
                let end = put_old.iter().position(|&byte| byte == 0).unwrap_or(0);
                &put_old[..end]
            });
            frame_ref.rax = match (read_new, read_old) {
                (Some(new_root), Some(put_old)) => {
                    match (
                        core::str::from_utf8(new_root),
                        core::str::from_utf8(put_old),
                    ) {
                        (Ok(new_root), Ok(put_old)) => {
                            match crate::vfs::pivot_root_to(new_root, put_old) {
                                Ok(()) => 0,
                                Err(error) => error.to_errno() as u64,
                            }
                        }
                        _ => (-22i64) as u64,
                    }
                }
                _ => (-14i64) as u64, // EFAULT
            };
            frame
        }
        SYS_CLOSE => {
            let Ok(fd) = usize::try_from(frame_ref.rdi) else {
                frame_ref.rax = (-9i64) as u64;
                return frame;
            };
            frame_ref.rax = if process::close_descriptor(fd) {
                0
            } else {
                (-9i64) as u64
            };
            frame
        }
        SYS_MKDIR | SYS_TOUCH | SYS_REMOVE => {
            let mut path_buffer = [0u8; 256];
            let path = match copy_user_path(frame_ref.rdi, frame_ref.rsi, &mut path_buffer) {
                Ok(path) => path,
                Err(error) => {
                    frame_ref.rax = error as u64;
                    return frame;
                }
            };
            let parent_access = if frame_ref.rax == SYS_MKDIR && frame_ref.rdx != 0 {
                recursive_parent_access_allowed(path, 3)
            } else {
                parent_access_allowed(path, 3)
            };
            if !parent_access {
                frame_ref.rax = (-13i64) as u64;
                return frame;
            }
            let (uid, gid, _) = process::current_credentials();
            let result = match frame_ref.rax {
                SYS_MKDIR if frame_ref.rdx != 0 => {
                    crate::ramfs::create_directory_with_parents_as(path, true, uid, gid)
                }
                SYS_MKDIR => crate::ramfs::create_directory_as(path, uid, gid),
                SYS_TOUCH => crate::ramfs::create_file_as(path, uid, gid),
                SYS_REMOVE => crate::ramfs::remove(path, frame_ref.rdx != 0),
                _ => unreachable!(),
            };
            frame_ref.rax = result.map_or_else(fs_error_code, |_| 0) as u64;
            frame
        }
        _ => {
            frame_ref.rax = (-38i64) as u64;
            frame
        }
    }
}

fn copy_user_path<'a>(
    address: u64,
    length: u64,
    buffer: &'a mut [u8; 256],
) -> Result<&'a str, i64> {
    let length = usize::try_from(length).map_err(|_| -22i64)?;
    if length == 0 || length > buffer.len() {
        return Err(-22);
    }
    if !process::copy_from_current_user(address, &mut buffer[..length]) {
        return Err(-14);
    }
    core::str::from_utf8(&buffer[..length]).map_err(|_| -22)
}

fn open_file(frame: &mut TrapFrame) -> *mut TrapFrame {
    let mut path_buffer = [0u8; imagineos_abi::MAX_MUTABLE_PATH];
    let path = match copy_user_path(frame.rdi, frame.rsi, &mut path_buffer) {
        Ok(path) => path,
        Err(error) => {
            frame.rax = error as u64;
            return frame;
        }
    };
    let flags = frame.rdx;
    let known_flags =
        OPEN_READ | OPEN_WRITE | OPEN_CREATE | OPEN_TRUNCATE | OPEN_APPEND | OPEN_EXCLUSIVE;
    if flags & !known_flags != 0
        || flags & (OPEN_READ | OPEN_WRITE) == 0
        || flags & (OPEN_TRUNCATE | OPEN_APPEND) != 0 && flags & OPEN_WRITE == 0
        || flags & OPEN_TRUNCATE != 0 && flags & OPEN_APPEND != 0
        || flags & OPEN_EXCLUSIVE != 0 && flags & OPEN_CREATE == 0
    {
        frame.rax = (-22i64) as u64;
        return frame;
    }

    if crate::ramfs::is_directory(path) {
        frame.rax = (-21i64) as u64;
        return frame;
    }
    let exists = crate::ramfs::is_file(path);
    if exists && flags & OPEN_CREATE != 0 && flags & OPEN_EXCLUSIVE != 0 {
        frame.rax = (-17i64) as u64;
        return frame;
    }
    if exists
        && ((flags & OPEN_READ != 0 && !access_allowed(path, 4))
            || (flags & OPEN_WRITE != 0 && !access_allowed(path, 2)))
    {
        frame.rax = (-13i64) as u64;
        return frame;
    }
    if !exists && flags & OPEN_CREATE != 0 && !parent_access_allowed(path, 3) {
        frame.rax = (-13i64) as u64;
        return frame;
    }
    if !crate::ramfs::is_file(path) {
        if flags & OPEN_CREATE == 0 {
            frame.rax = (-2i64) as u64;
            return frame;
        }
        let (uid, gid, _) = process::current_credentials();
        if let Err(error) =
            crate::ramfs::create_file_with_mode_as(path, (frame.r10 as u16) & 0o777, uid, gid)
        {
            frame.rax = fs_error_code(error) as u64;
            return frame;
        }
    }
    if flags & OPEN_TRUNCATE != 0 {
        let (uid, gid, _) = process::current_credentials();
        if let Err(error) = crate::ramfs::write_file_as(path, &[], uid, gid) {
            frame.rax = fs_error_code(error) as u64;
            return frame;
        }
    }
    let offset = if flags & OPEN_APPEND != 0 {
        match crate::ramfs::file_len(path) {
            Ok(length) => length,
            Err(error) => {
                frame.rax = fs_error_code(error) as u64;
                return frame;
            }
        }
    } else {
        0
    };
    let descriptor = process::Descriptor::file(path, flags, offset);
    frame.rax = process::allocate_descriptor(descriptor).map_or((-24i64) as u64, |fd| fd as u64);
    frame
}

fn access_allowed(path: &str, requested: u16) -> bool {
    let (uid, gid, _) = process::current_credentials();
    if uid == 0 {
        return true;
    }
    if !ancestors_searchable(path, uid, gid) {
        return false;
    }
    let Some(metadata) = crate::ramfs::metadata(path) else {
        return false;
    };
    crate::permissions::allows(
        metadata.mode,
        metadata.uid,
        metadata.gid,
        uid,
        gid,
        requested,
    )
}

fn ancestors_searchable(path: &str, uid: u32, gid: u32) -> bool {
    let mut prefix = [0u8; imagineos_abi::MAX_MUTABLE_PATH];
    let mut length = 0usize;
    let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
    for component in parent
        .trim_matches('/')
        .split('/')
        .take_while(|part| !part.is_empty())
    {
        if length != 0 {
            prefix[length] = b'/';
            length += 1;
        }
        if length + component.len() > prefix.len() {
            return false;
        }
        prefix[length..length + component.len()].copy_from_slice(component.as_bytes());
        length += component.len();
        let directory = core::str::from_utf8(&prefix[..length]).unwrap_or("");
        if !crate::ramfs::metadata(directory).is_some_and(|metadata| {
            if !metadata.is_directory {
                return false;
            }
            crate::permissions::allows(metadata.mode, metadata.uid, metadata.gid, uid, gid, 1)
        }) {
            return false;
        }
    }
    true
}

fn parent_access_allowed(path: &str, requested: u16) -> bool {
    let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
    access_allowed(parent, requested)
}

fn recursive_parent_access_allowed(path: &str, requested: u16) -> bool {
    let (uid, gid, _) = process::current_credentials();
    if uid == 0 {
        return true;
    }
    let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
    let mut prefix = [0u8; imagineos_abi::MAX_MUTABLE_PATH];
    let mut length = 0usize;
    let mut last_existing = crate::ramfs::metadata("");
    for component in parent.trim_matches('/').split('/') {
        if component.is_empty() {
            continue;
        }
        if length != 0 {
            prefix[length] = b'/';
            length += 1;
        }
        if length + component.len() > prefix.len() {
            return false;
        }
        prefix[length..length + component.len()].copy_from_slice(component.as_bytes());
        length += component.len();
        let current = core::str::from_utf8(&prefix[..length]).unwrap_or("");
        match crate::ramfs::metadata(current) {
            Some(metadata) if metadata.is_directory => {
                if !mode_allows(metadata, uid, gid, 1) {
                    return false;
                }
                last_existing = Some(metadata);
            }
            Some(_) => return false,
            None => break,
        }
    }
    last_existing.is_some_and(|metadata| mode_allows(metadata, uid, gid, requested))
}

fn mode_allows(metadata: crate::dfs::Metadata, uid: u32, gid: u32, requested: u16) -> bool {
    crate::permissions::allows(
        metadata.mode,
        metadata.uid,
        metadata.gid,
        uid,
        gid,
        requested,
    )
}

/// `fb_info(&FramebufferInfo)`: descreve o framebuffer ativo, sem mapeá-lo.
///
/// O servidor de display usa isto para conhecer dimensões e formato antes de
/// decidir onde mapear a tela.
fn framebuffer_info(frame: &mut TrapFrame) -> *mut TrapFrame {
    let Some(description) = crate::framebuffer::description() else {
        frame.rax = (-19i64) as u64; // ENODEV
        return frame;
    };
    let mut info = imagineos_abi::FramebufferInfo {
        address: 0,
        physical_address: description.physical_address,
        width: description.width as u32,
        height: description.height as u32,
        pitch: description.pitch as u32,
        red_shift: description.red_shift,
        green_shift: description.green_shift,
        blue_shift: description.blue_shift,
        reserved: 0,
    };
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(
            (&mut info as *mut imagineos_abi::FramebufferInfo).cast::<u8>(),
            core::mem::size_of::<imagineos_abi::FramebufferInfo>(),
        )
    };
    frame.rax = if process::copy_to_current_user(frame.rdi, bytes) {
        0
    } else {
        (-14i64) as u64
    };
    frame
}

/// `fb_map(&FramebufferInfo)`: mapeia a memória de vídeo no processo atual e
/// preenche `address` com o endereço virtual resultante.
///
/// O endereço base é escolhido pelo kernel na área alta de usuário, livre entre
/// a pilha e o fim do espaço de usuário.
fn framebuffer_map(frame: &mut TrapFrame) -> *mut TrapFrame {
    const FB_BASE: u64 = 0x0000_4000_0000_0000;
    let Some(description) = crate::framebuffer::description() else {
        frame.rax = (-19i64) as u64; // ENODEV
        return frame;
    };
    let length = description
        .pitch
        .saturating_mul(description.height);
    let mapped = process::with_current_space(|space| {
        space.map_framebuffer_range(FB_BASE, description.physical_address, length)
    });
    let Some(address) = mapped else {
        frame.rax = (-12i64) as u64; // ENOMEM
        return frame;
    };

    let mut info = imagineos_abi::FramebufferInfo {
        address,
        physical_address: description.physical_address,
        width: description.width as u32,
        height: description.height as u32,
        pitch: description.pitch as u32,
        red_shift: description.red_shift,
        green_shift: description.green_shift,
        blue_shift: description.blue_shift,
        reserved: 0,
    };
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(
            (&mut info as *mut imagineos_abi::FramebufferInfo).cast::<u8>(),
            core::mem::size_of::<imagineos_abi::FramebufferInfo>(),
        )
    };
    frame.rax = if process::copy_to_current_user(frame.rdi, bytes) {
        0
    } else {
        (-14i64) as u64
    };
    frame
}

/// `surface_create(&SurfaceInfo)`: registra uma superfície e mapeia o buffer.
fn surface_create(frame: &mut TrapFrame) -> *mut TrapFrame {
    let mut info = imagineos_abi::SurfaceInfo::default();
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(
            (&mut info as *mut imagineos_abi::SurfaceInfo).cast::<u8>(),
            core::mem::size_of::<imagineos_abi::SurfaceInfo>(),
        )
    };
    if !process::copy_from_current_user(frame.rdi, bytes) {
        frame.rax = (-14i64) as u64; // EFAULT
        return frame;
    }
    frame.rax = match crate::surface::create(&mut info) {
        Ok(()) => {
            let out = unsafe {
                core::slice::from_raw_parts(
                    (&info as *const imagineos_abi::SurfaceInfo).cast::<u8>(),
                    core::mem::size_of::<imagineos_abi::SurfaceInfo>(),
                )
            };
            if process::copy_to_current_user(frame.rdi, out) {
                0
            } else {
                (-14i64) as u64
            }
        }
        Err(code) => code as u64,
    };
    frame
}

/// `surface_present(id, shown)`: mostra/esconde a superfície.
fn surface_present(frame: &mut TrapFrame) -> *mut TrapFrame {
    frame.rax = match crate::surface::present(frame.rdi as u32, frame.rsi != 0) {
        Ok(()) => 0,
        Err(code) => code as u64,
    };
    frame
}

/// `surface_flush(id)`: publica o frame escrito no buffer.
fn surface_flush(frame: &mut TrapFrame) -> *mut TrapFrame {
    frame.rax = match crate::surface::flush(frame.rdi as u32) {
        Ok(()) => 0,
        Err(code) => code as u64,
    };
    frame
}

/// `surface_destroy(id)`: remove a superfície.
fn surface_destroy(frame: &mut TrapFrame) -> *mut TrapFrame {
    frame.rax = match crate::surface::destroy(frame.rdi as u32) {
        Ok(()) => 0,
        Err(code) => code as u64,
    };
    frame
}

/// `surface_poll(&SurfaceEvent)`: retira um evento do dono de superfícies.
fn surface_poll(frame: &mut TrapFrame) -> *mut TrapFrame {
    let Some(event) = crate::surface::pop_event() else {
        frame.rax = 0;
        return frame;
    };
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (&event as *const imagineos_abi::SurfaceEvent).cast::<u8>(),
            core::mem::size_of::<imagineos_abi::SurfaceEvent>(),
        )
    };
    frame.rax = if process::copy_to_current_user(frame.rdi, bytes) {
        1
    } else {
        (-14i64) as u64
    };
    frame
}

/// `surface_snapshot(&[SurfaceSnapshot], max)`: lista as superfícies visíveis
/// e mapeia os buffers no processo atual (o servidor de display).
fn surface_snapshot(frame: &mut TrapFrame) -> *mut TrapFrame {
    const MAX_OUTPUT: usize = imagineos_abi::MAX_SURFACES;
    let capacity = (frame.rsi as usize).min(MAX_OUTPUT);
    if frame.rdi == 0 || capacity == 0 {
        frame.rax = (-22i64) as u64; // EINVAL
        return frame;
    }
    let mut output = [imagineos_abi::SurfaceSnapshot::default(); MAX_OUTPUT];
    let count = crate::surface::snapshot_mapped(&mut output[..capacity]);
    let bytes = unsafe {
        core::slice::from_raw_parts(
            output.as_ptr().cast::<u8>(),
            count * core::mem::size_of::<imagineos_abi::SurfaceSnapshot>(),
        )
    };
    frame.rax = if process::copy_to_current_user(frame.rdi, bytes) {
        count as u64
    } else {
        (-14i64) as u64
    };
    frame
}

/// `surface_send_event(&SurfaceEvent)`: entrega um evento ao dono.
fn surface_send_event(frame: &mut TrapFrame) -> *mut TrapFrame {
    let mut event = imagineos_abi::SurfaceEvent::default();
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(
            (&mut event as *mut imagineos_abi::SurfaceEvent).cast::<u8>(),
            core::mem::size_of::<imagineos_abi::SurfaceEvent>(),
        )
    };
    if !process::copy_from_current_user(frame.rdi, bytes) {
        frame.rax = (-14i64) as u64;
        return frame;
    }
    frame.rax = if crate::surface::push_event_to_surface(event.surface, event) {
        0
    } else {
        (-2i64) as u64 // ENOENT
    };
    frame
}

/// `input_poll(&InputEvent)`: retira um evento de entrada.
///
/// Devolve `1` quando um evento foi escrito, `0` quando a fila está vazia.
/// O servidor de display chama isto em laço, cedendo a CPU entre as chamadas.
fn input_poll(frame: &mut TrapFrame) -> *mut TrapFrame {
    // Quando o servidor captura o console, o próprio poll alimenta a fila com
    // teclas PS/2. Fora do modo display, quem lê é o caminho antigo `read_char`.
    if crate::input::is_captured() {
        for _ in 0..16 {
            let interrupts_were_enabled = disable_interrupts();
            let key = unsafe { (&mut *KEYBOARD.0.get()).poll_char() };
            restore_interrupts(interrupts_were_enabled);
            let Some(key) = key else { break };
            crate::input::push_key(key);
        }
    }
    // Aproveita a chamada para drenar o mouse: o kernel não tem IRQ para a
    // porta auxiliar, e o servidor de display é quem decide a cadência com que
    // consulta os eventos. `mouse::poll` acumula todos os pacotes disponíveis,
    // e `push_pointer` coalesce o movimento, de modo que o WM sempre recebe a
    // posição mais recente em vez de uma fila de posições antigas.
    while let Some(report) = crate::mouse::poll() {
        if report.delta_x != 0 || report.delta_y != 0 {
            crate::input::push_pointer(report.delta_x, report.delta_y);
        }
        crate::input::push_pointer_button(1, report.left);
        crate::input::push_pointer_button(2, report.right);
    }
    let Some(event) = crate::input::poll() else {
        frame.rax = 0;
        return frame;
    };
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (&event as *const imagineos_abi::InputEvent).cast::<u8>(),
            core::mem::size_of::<imagineos_abi::InputEvent>(),
        )
    };
    frame.rax = if process::copy_to_current_user(frame.rdi, bytes) {
        1
    } else {
        (-14i64) as u64
    };
    frame
}

/// `pipe(int fds[2])`: cria um pipe e escreve os dois descritores no array do
/// usuário, com o lado de leitura primeiro (posições 0 e 1), como no POSIX.
fn create_pipe(frame: &mut TrapFrame) -> *mut TrapFrame {
    let id = match crate::ipc::create() {
        Ok(id) => id,
        Err(error) => {
            frame.rax = error.to_errno() as u64;
            return frame;
        }
    };
    let read_end = process::allocate_descriptor(process::Descriptor::pipe(
        process::DescriptorKind::PipeRead,
        id,
    ));
    let write_end = process::allocate_descriptor(process::Descriptor::pipe(
        process::DescriptorKind::PipeWrite,
        id,
    ));
    let (Some(read_end), Some(write_end)) = (read_end, write_end) else {
        // Roll back whatever was allocated so the pipe does not leak.
        if let Some(fd) = read_end {
            process::close_descriptor(fd);
        }
        if let Some(fd) = write_end {
            process::close_descriptor(fd);
        }
        crate::ipc::close_reader(id);
        crate::ipc::close_writer(id);
        frame.rax = (-24i64) as u64; // EMFILE
        return frame;
    };
    let mut endpoints = [read_end as i32, write_end as i32];
    let bytes = unsafe {
        core::slice::from_raw_parts(
            endpoints.as_mut_ptr().cast::<u8>(),
            core::mem::size_of_val(&endpoints),
        )
    };
    if !process::copy_to_current_user(frame.rdi, bytes) {
        process::close_descriptor(read_end);
        process::close_descriptor(write_end);
        frame.rax = (-14i64) as u64;
        return frame;
    }
    frame.rax = 0;
    frame
}

/// `spawn(path, argv, envp, io)`: como `exec`, mas o filho recebe descritores
/// padrão escolhidos pelo chamador. É a primitiva que o shell usa para montar
/// um estágio de pipeline sem `fork`.
fn spawn_with_io(frame: &mut TrapFrame) -> *mut TrapFrame {
    let length = frame.rsi as usize;
    if length == 0 || length > imagineos_abi::MAX_EXEC_ITEM_SIZE {
        frame.rax = (-22i64) as u64;
        return frame;
    }
    let mut path = [0u8; 128];
    if !process::copy_from_current_user(frame.rdi, &mut path[..length]) {
        frame.rax = (-14i64) as u64;
        return frame;
    }
    if path[..length].contains(&0) {
        frame.rax = (-22i64) as u64;
        return frame;
    }
    let Ok(path) = core::str::from_utf8(&path[..length]) else {
        frame.rax = (-22i64) as u64;
        return frame;
    };
    // Layout dos argumentos de `SYS_SPAWN` na ABI (ver `userland/api`):
    //
    //   rdi = ponteiro do caminho      rsi = comprimento do caminho
    //   rdx = ponteiro dos argumentos  r10 = quantidade de argumentos
    //   r8  = ponteiro de `SpawnIo`    r9  = ponteiro do ambiente
    //
    // Atenção: o 4º argumento chega em `r10`, não em `rcx` — `rcx` é usado
    // pela instrução `int 0x80` para o endereço de retorno, por isso a ABI
    // desvia esse parâmetro. Ler `rcx` aqui fazia a cópia do array de
    // argumentos falhar e todo comando externo terminava em "falha ao
    // executar o comando".
    let mut io = imagineos_abi::SpawnIo::default();
    let io_bytes = unsafe {
        core::slice::from_raw_parts_mut(
            (&mut io as *mut imagineos_abi::SpawnIo).cast::<u8>(),
            core::mem::size_of::<imagineos_abi::SpawnIo>(),
        )
    };
    let io_pointer = frame.r8;
    if io_pointer != 0 && !process::copy_from_current_user(io_pointer, io_bytes) {
        frame.rax = (-14i64) as u64;
        return frame;
    }
    // O ambiente vem dentro de `SpawnIo`: os seis registradores já estão
    // ocupados (path, len, argv, argc, io, —). `r9` não é usado.
    let environment_pointer = io.environment;
    let environment_count = io.environment_count as usize;
    let argument_count = frame.r10 as usize;
    if argument_count > process::MAX_EXEC_ARGS {
        frame.rax = (-7i64) as u64;
        return frame;
    }
    let mut user_arguments = [process::UserArg::EMPTY; process::MAX_EXEC_ARGS];
    let metadata = unsafe {
        core::slice::from_raw_parts_mut(
            user_arguments.as_mut_ptr().cast::<u8>(),
            argument_count * core::mem::size_of::<process::UserArg>(),
        )
    };
    if argument_count != 0 && !process::copy_from_current_user(frame.rdx, metadata) {
        frame.rax = (-14i64) as u64;
        return frame;
    }
    let mut argument_storage = [[0u8; 128]; process::MAX_EXEC_ARGS];
    let mut argument_lengths = [0usize; process::MAX_EXEC_ARGS];
    for index in 0..argument_count {
        let Ok(item_length) = usize::try_from(user_arguments[index].length) else {
            frame.rax = (-7i64) as u64;
            return frame;
        };
        if item_length > argument_storage[index].len() {
            frame.rax = (-7i64) as u64;
            return frame;
        }
        if !process::copy_from_current_user(
            user_arguments[index].address,
            &mut argument_storage[index][..item_length],
        ) {
            frame.rax = (-14i64) as u64;
            return frame;
        }
        if argument_storage[index][..item_length].contains(&0) {
            frame.rax = (-22i64) as u64;
            return frame;
        }
        argument_lengths[index] = item_length;
    }
    let mut arguments: [&[u8]; process::MAX_EXEC_ARGS] = [&[]; process::MAX_EXEC_ARGS];
    for index in 0..argument_count {
        arguments[index] = &argument_storage[index][..argument_lengths[index]];
    }

    // Ambiente: mesma codificação de `UserArg` usada pelos argumentos.
    if environment_count > process::MAX_EXEC_ENV {
        frame.rax = (-7i64) as u64;
        return frame;
    }
    let mut user_environment = [process::UserArg::EMPTY; process::MAX_EXEC_ENV];
    let environment_metadata = unsafe {
        core::slice::from_raw_parts_mut(
            user_environment.as_mut_ptr().cast::<u8>(),
            environment_count * core::mem::size_of::<process::UserArg>(),
        )
    };
    if environment_count != 0
        && !process::copy_from_current_user(environment_pointer, environment_metadata)
    {
        frame.rax = (-14i64) as u64;
        return frame;
    }
    let mut environment_storage = [[0u8; 128]; process::MAX_EXEC_ENV];
    let mut environment_lengths = [0usize; process::MAX_EXEC_ENV];
    for index in 0..environment_count {
        let Ok(item_length) = usize::try_from(user_environment[index].length) else {
            frame.rax = (-7i64) as u64;
            return frame;
        };
        if item_length > environment_storage[index].len() {
            frame.rax = (-7i64) as u64;
            return frame;
        }
        if !process::copy_from_current_user(
            user_environment[index].address,
            &mut environment_storage[index][..item_length],
        ) {
            frame.rax = (-14i64) as u64;
            return frame;
        }
        if environment_storage[index][..item_length].contains(&0) {
            frame.rax = (-22i64) as u64;
            return frame;
        }
        environment_lengths[index] = item_length;
    }
    let mut environment: [&[u8]; process::MAX_EXEC_ENV] = [&[]; process::MAX_EXEC_ENV];
    for index in 0..environment_count {
        environment[index] = &environment_storage[index][..environment_lengths[index]];
    }

    process::spawn_current_with_io(
        path,
        &arguments[..argument_count],
        &environment[..environment_count],
        &io,
        frame,
    )
}

/// Lê um vetor de `UserArg` e copia cada item para armazenamento próprio.
///
/// Vale tanto para `argv` quanto para `envp`, que têm a mesma codificação.
/// `records` é dimensionado pelo maior vetor possível; `count` limita o uso.
fn load_vector(
    pointer: u64,
    count: usize,
    limit: usize,
    storage: &mut [[u8; 128]],
    lengths: &mut [usize],
) -> Result<usize, i64> {
    if count > limit || count > storage.len() || count > lengths.len() {
        return Err(-7);
    }
    if count == 0 {
        return Ok(0);
    }
    let mut records = [UserArg::EMPTY; process::MAX_EXEC_ARGS];
    let metadata = unsafe {
        core::slice::from_raw_parts_mut(
            records.as_mut_ptr().cast::<u8>(),
            count * core::mem::size_of::<UserArg>(),
        )
    };
    if !process::copy_from_current_user(pointer, metadata) {
        return Err(-14);
    }
    for index in 0..count {
        let Ok(length) = usize::try_from(records[index].length) else {
            return Err(-7);
        };
        if length > storage[index].len() {
            return Err(-7);
        }
        if !process::copy_from_current_user(
            records[index].address,
            &mut storage[index][..length],
        ) {
            return Err(-14);
        }
        if storage[index][..length].contains(&0) {
            return Err(-22);
        }
        lengths[index] = length;
    }
    Ok(count)
}

/// Decodifica os argumentos comuns a `SYS_EXEC` e `SYS_EXECVE`.
///
/// Layout esperado no `TrapFrame` (igual ao de `execve(2)`):
/// `rdi = path`, `rsi = path_len`, `rdx = argv`, `r10 = argc`,
/// `r8 = envp`, `r9 = envc`.
///
/// Os buffers vivem na estrutura para que os empréstimos `&[u8]` devolvidos
/// por [`arguments`](Self::arguments) sejam válidos enquanto o chamador os usa.
struct ExecArguments {
    path: [u8; 128],
    path_length: usize,
    argument_storage: [[u8; 128]; process::MAX_EXEC_ARGS],
    argument_lengths: [usize; process::MAX_EXEC_ARGS],
    argument_count: usize,
    environment_storage: [[u8; 128]; process::MAX_EXEC_ENV],
    environment_lengths: [usize; process::MAX_EXEC_ENV],
    environment_count: usize,
}

impl ExecArguments {
    const fn new() -> Self {
        Self {
            path: [0; 128],
            path_length: 0,
            argument_storage: [[0; 128]; process::MAX_EXEC_ARGS],
            argument_lengths: [0; process::MAX_EXEC_ARGS],
            argument_count: 0,
            environment_storage: [[0; 128]; process::MAX_EXEC_ENV],
            environment_lengths: [0; process::MAX_EXEC_ENV],
            environment_count: 0,
        }
    }

    fn load(&mut self, frame: &TrapFrame) -> Result<(), i64> {
        self.load_path(frame.rdi, frame.rsi)?;
        // `load_vector` é função livre para não conflitar com o empréstimo dos
        // campos de `self` que ela própria preenche.
        let argument_count = load_vector(
            frame.rdx,
            frame.r10 as usize,
            process::MAX_EXEC_ARGS,
            &mut self.argument_storage,
            &mut self.argument_lengths,
        )?;
        let environment_count = load_vector(
            frame.r8,
            frame.r9 as usize,
            process::MAX_EXEC_ENV,
            &mut self.environment_storage,
            &mut self.environment_lengths,
        )?;
        self.argument_count = argument_count;
        self.environment_count = environment_count;
        Ok(())
    }

    fn load_path(&mut self, pointer: u64, length: u64) -> Result<(), i64> {
        let length = usize::try_from(length).map_err(|_| -22i64)?;
        if length == 0 || length > imagineos_abi::MAX_EXEC_ITEM_SIZE || length > self.path.len() {
            return Err(-22);
        }
        if !process::copy_from_current_user(pointer, &mut self.path[..length]) {
            return Err(-14);
        }
        if self.path[..length].contains(&0) {
            return Err(-22);
        }
        self.path_length = length;
        Ok(())
    }

    fn path(&self) -> &str {
        // A codificação foi validada no `load_path`.
        core::str::from_utf8(&self.path[..self.path_length]).unwrap_or("")
    }

    /// Quantidade real de argumentos (o array tem o tamanho máximo).
    fn argument_count(&self) -> usize {
        self.argument_count
    }

    fn environment_count(&self) -> usize {
        self.environment_count
    }

    fn arguments(&self) -> [&[u8]; process::MAX_EXEC_ARGS] {
        let mut arguments: [&[u8]; process::MAX_EXEC_ARGS] = [&[]; process::MAX_EXEC_ARGS];
        for index in 0..self.argument_count {
            arguments[index] = &self.argument_storage[index][..self.argument_lengths[index]];
        }
        arguments
    }

    fn environment(&self) -> [&[u8]; process::MAX_EXEC_ENV] {
        let mut environment: [&[u8]; process::MAX_EXEC_ENV] = [&[]; process::MAX_EXEC_ENV];
        for index in 0..self.environment_count {
            environment[index] = &self.environment_storage[index][..self.environment_lengths[index]];
        }
        environment
    }
}

fn read_fd(frame: &mut TrapFrame) -> *mut TrapFrame {
    let (Ok(fd), Ok(capacity)) = (usize::try_from(frame.rdi), usize::try_from(frame.rdx)) else {
        frame.rax = (-22i64) as u64;
        return frame;
    };
    if capacity > imagineos_abi::MAX_READ_BUFFER {
        frame.rax = (-22i64) as u64;
        return frame;
    }
    if capacity == 0 {
        frame.rax = 0;
        return frame;
    }
    let Some(mut descriptor) = process::descriptor(fd) else {
        frame.rax = (-9i64) as u64;
        return frame;
    };
    if descriptor.kind == process::DescriptorKind::File {
        let path = core::str::from_utf8(&descriptor.path[..descriptor.path_length])
            .expect("descriptor path is validated on open");
        if !access_allowed(path, 4) {
            frame.rax = (-13i64) as u64;
            return frame;
        }
    }
    let mut buffer = [0u8; imagineos_abi::MAX_READ_BUFFER];
    let length = match descriptor.kind {
        process::DescriptorKind::Stdin => {
            if descriptor.input_offset == descriptor.input_length {
                let mut encoded = [0u8; 4];
                let character = read_character();
                let value = char::from_u32(character).unwrap_or('\u{fffd}');
                let bytes = value.encode_utf8(&mut encoded).as_bytes();
                descriptor.input[..bytes.len()].copy_from_slice(bytes);
                descriptor.input_length = bytes.len();
                descriptor.input_offset = 0;
            }
            let length = capacity.min(descriptor.input_length - descriptor.input_offset);
            buffer[..length].copy_from_slice(
                &descriptor.input[descriptor.input_offset..descriptor.input_offset + length],
            );
            descriptor.input_offset += length;
            length
        }
        process::DescriptorKind::File if descriptor.flags & OPEN_READ != 0 => {
            let path = core::str::from_utf8(&descriptor.path[..descriptor.path_length])
                .expect("descriptor path is validated on open");
            match crate::ramfs::read_at(path, descriptor.offset, &mut buffer[..capacity]) {
                Ok(length) => {
                    descriptor.offset += length;
                    length
                }
                Err(error) => {
                    frame.rax = fs_error_code(error) as u64;
                    return frame;
                }
            }
        }
        process::DescriptorKind::File => {
            frame.rax = (-9i64) as u64;
            return frame;
        }
        process::DescriptorKind::PipeRead => {
            match crate::ipc::read(descriptor.pipe, &mut buffer[..capacity]) {
                Ok(length) => {
                    // Keep the descriptor untouched (pipes are not seekable).
                    if !process::copy_to_current_user(frame.rsi, &buffer[..length]) {
                        frame.rax = (-14i64) as u64;
                    } else {
                        frame.rax = length as u64;
                    }
                    return frame;
                }
                Err(error) => {
                    frame.rax = error.to_errno() as u64;
                    return frame;
                }
            }
        }
        _ => {
            frame.rax = (-9i64) as u64;
            return frame;
        }
    };
    if !process::copy_to_current_user(frame.rsi, &buffer[..length]) {
        frame.rax = (-14i64) as u64;
    } else if !process::update_descriptor(fd, descriptor) {
        frame.rax = (-9i64) as u64;
    } else {
        frame.rax = length as u64;
    }
    frame
}

fn seek_fd(frame: &mut TrapFrame) -> *mut TrapFrame {
    let Ok(fd) = usize::try_from(frame.rdi) else {
        frame.rax = (-9i64) as u64;
        return frame;
    };
    let Some(mut descriptor) = process::descriptor(fd) else {
        frame.rax = (-9i64) as u64;
        return frame;
    };
    if descriptor.kind != process::DescriptorKind::File {
        frame.rax = (-29i64) as u64;
        return frame;
    }
    let base = match frame.rdx {
        0 => 0i128,
        1 => descriptor.offset as i128,
        2 => {
            let path = core::str::from_utf8(&descriptor.path[..descriptor.path_length])
                .expect("descriptor path is validated on open");
            match crate::ramfs::file_len(path) {
                Ok(length) => length as i128,
                Err(error) => {
                    frame.rax = fs_error_code(error) as u64;
                    return frame;
                }
            }
        }
        _ => {
            frame.rax = (-22i64) as u64;
            return frame;
        }
    };
    let offset = base + (frame.rsi as i64) as i128;
    let Ok(offset) = usize::try_from(offset) else {
        frame.rax = (-22i64) as u64;
        return frame;
    };
    descriptor.offset = offset;
    if process::update_descriptor(fd, descriptor) {
        frame.rax = offset as u64;
    } else {
        frame.rax = (-9i64) as u64;
    }
    frame
}

fn fstat_fd(frame: &mut TrapFrame) -> *mut TrapFrame {
    let Ok(fd) = usize::try_from(frame.rdi) else {
        frame.rax = (-9i64) as u64;
        return frame;
    };
    let Some(descriptor) = process::descriptor(fd) else {
        frame.rax = (-9i64) as u64;
        return frame;
    };
    if descriptor.kind != process::DescriptorKind::File {
        frame.rax = (-9i64) as u64;
        return frame;
    }
    let path = core::str::from_utf8(&descriptor.path[..descriptor.path_length])
        .expect("descriptor path is validated on open");
    let Some(metadata) = crate::ramfs::metadata(path) else {
        frame.rax = (-2i64) as u64;
        return frame;
    };
    let result = UserStat {
        size: metadata.size,
        mode: metadata.mode as u32,
        uid: metadata.uid,
        gid: metadata.gid,
        kind: u32::from(metadata.is_directory),
    };
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (&result as *const UserStat).cast::<u8>(),
            core::mem::size_of::<UserStat>(),
        )
    };
    frame.rax = if process::copy_to_current_user(frame.rsi, bytes) {
        0
    } else {
        (-14i64) as u64
    };
    frame
}

fn gettimeofday(frame: &mut TrapFrame) -> *mut TrapFrame {
    #[repr(C)]
    struct UserTimeval {
        seconds: i64,
        microseconds: i64,
    }
    let Some(microseconds) = crate::time::elapsed_microseconds() else {
        frame.rax = (-5i64) as u64;
        return frame;
    };
    let value = UserTimeval {
        seconds: (microseconds / 1_000_000) as i64,
        microseconds: (microseconds % 1_000_000) as i64,
    };
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (&value as *const UserTimeval).cast::<u8>(),
            core::mem::size_of::<UserTimeval>(),
        )
    };
    frame.rax = if process::copy_to_current_user(frame.rdi, bytes) {
        0
    } else {
        (-14i64) as u64
    };
    frame
}

fn write_fd(frame: &mut TrapFrame) -> *mut TrapFrame {
    let (Ok(fd), Ok(length)) = (usize::try_from(frame.rdi), usize::try_from(frame.rdx)) else {
        frame.rax = (-22i64) as u64;
        return frame;
    };
    if length > imagineos_abi::MAX_READ_BUFFER {
        frame.rax = (-22i64) as u64;
        return frame;
    }
    let Some(mut descriptor) = process::descriptor(fd) else {
        frame.rax = (-9i64) as u64;
        return frame;
    };
    if descriptor.kind == process::DescriptorKind::File {
        let path = core::str::from_utf8(&descriptor.path[..descriptor.path_length])
            .expect("descriptor path is validated on open");
        if !access_allowed(path, 2) {
            frame.rax = (-13i64) as u64;
            return frame;
        }
    }
    let mut buffer = [0u8; imagineos_abi::MAX_READ_BUFFER];
    if !process::copy_from_current_user(frame.rsi, &mut buffer[..length]) {
        frame.rax = (-14i64) as u64;
        return frame;
    }
    match descriptor.kind {
        process::DescriptorKind::Stdout | process::DescriptorKind::Stderr => {
            crate::console_write_bytes(&buffer[..length]);
            frame.rax = length as u64;
        }
        process::DescriptorKind::PipeWrite => match crate::ipc::write(descriptor.pipe, &buffer[..length]) {
            Ok(written) => frame.rax = written as u64,
            Err(error) => frame.rax = error.to_errno() as u64,
        },
        process::DescriptorKind::File if descriptor.flags & OPEN_WRITE != 0 => {
            let path = core::str::from_utf8(&descriptor.path[..descriptor.path_length])
                .expect("descriptor path is validated on open");
            let offset = if descriptor.flags & OPEN_APPEND != 0 {
                match crate::ramfs::file_len(path) {
                    Ok(offset) => offset,
                    Err(error) => {
                        frame.rax = fs_error_code(error) as u64;
                        return frame;
                    }
                }
            } else {
                descriptor.offset
            };
            match crate::ramfs::write_at(path, offset, &buffer[..length]) {
                Ok(written) => {
                    descriptor.offset = offset + written;
                    if !process::update_descriptor(fd, descriptor) {
                        frame.rax = (-9i64) as u64;
                    } else {
                        frame.rax = written as u64;
                    }
                }
                Err(error) => frame.rax = fs_error_code(error) as u64,
            }
        }
        _ => frame.rax = (-9i64) as u64,
    }
    frame
}

/// Maps a filesystem error to the negative errno returned by the syscall ABI.
///
/// The table itself lives in one place, `VfsError::to_errno`, so the VFS
/// backends and the syscall layer can never drift apart.
fn fs_error_code(error: crate::ramfs::FsError) -> i64 {
    crate::vfs::VfsError::from(error).to_errno()
}

fn read_character() -> u32 {
    const BLINK_INTERVAL: u64 = 750_000_000;
    let mut last_blink = read_tsc();
    framebuffer::set_cursor_visible(true);
    let mut cursor_visible = true;
    loop {
        // O estado do teclado (shift, dead keys, prefixo E0) é um único
        // `Keyboard` global sem lock. Se o timer preemptivo interromper esta
        // leitura, outro processo pode entrar em `poll_char` e embaralhar esse
        // estado — foi assim que caracteres do login sumiram. Mantemos as
        // interrupções desligadas durante a leitura da porta para garantir que
        // a sequência "ler status, ler dado, atualizar estado" seja atômica.
        let character = {
            let interrupts_were_enabled = disable_interrupts();
            let polled = unsafe { (&mut *KEYBOARD.0.get()).poll_char() };
            restore_interrupts(interrupts_were_enabled);
            polled
        };
        if let Some(character) = character {
            // Com o servidor de display no comando, o caractere vai para a fila
            // de eventos dele em vez de ser entregue à linha de comando do
            // kernel. Assim o terminal de texto e o display não disputam a
            // mesma tecla.
            if crate::input::is_captured() {
                crate::input::push_key(character);
                continue;
            }
            framebuffer::set_cursor_visible(false);
            if character == crate::keyboard::KEY_INTERRUPT {
                let _ = process::send_signal(process::current_pid(), imagineos_abi::SIGNAL_INT);
                return b'\n' as u32;
            }
            return character as u32;
        }
        if let Some(character) = serial_read_char() {
            framebuffer::set_cursor_visible(false);
            if character == '\u{3}' {
                let _ = process::send_signal(process::current_pid(), imagineos_abi::SIGNAL_INT);
                return b'\n' as u32;
            }
            return character as u32;
        }
        let now = read_tsc();
        if now.wrapping_sub(last_blink) >= BLINK_INTERVAL {
            cursor_visible = !cursor_visible;
            framebuffer::set_cursor_visible(cursor_visible);
            last_blink = now;
        }
        // Nada a fazer: gira. As interrupções seguem ligadas, então o timer
        // pode preemptar; `poll_char` é atômico em relação a elas.
        core::hint::spin_loop();
    }
}

/// Desabilita interrupções e devolve o estado anterior de `RFLAGS.IF`.
fn disable_interrupts() -> bool {
    let flags: u64;
    unsafe {
        asm!("pushfq", "pop {}", out(reg) flags, options(nomem, preserves_flags));
        asm!("cli", options(nomem, nostack, preserves_flags));
    }
    flags & (1 << 9) != 0
}

/// Restaura `RFLAGS.IF` conforme o valor devolvido por [`disable_interrupts`].
fn restore_interrupts(was_enabled: bool) {
    if was_enabled {
        unsafe {
            asm!("sti", options(nomem, nostack, preserves_flags));
        }
    }
}

fn read_tsc() -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!(
            "rdtsc",
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags)
        );
    }
    ((high as u64) << 32) | low as u64
}

fn serial_read_char() -> Option<char> {
    unsafe {
        let decoder = &mut *SERIAL_UTF8.0.get();
        if let Some(character) = decoder.push_pending() {
            return Some(character);
        }
        if in_port(0x3fd) & 1 != 0 {
            decoder.push(in_port(0x3f8))
        } else {
            None
        }
    }
}

unsafe fn in_port(port: u16) -> u8 {
    let value: u8;
    asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    value
}
