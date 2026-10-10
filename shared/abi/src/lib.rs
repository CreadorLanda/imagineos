#![no_std]

pub const ABI_VERSION: u64 = 3;

pub const SYS_WRITE: u64 = 1;
pub const SYS_READ: u64 = 2;
pub const SYS_YIELD: u64 = 3;
pub const SYS_EXIT: u64 = 4;
pub const SYS_GETPID: u64 = 5;
pub const SYS_CLEAR: u64 = 6;
pub const SYS_EXEC: u64 = 7;
pub const SYS_ISDIR: u64 = 8;
pub const SYS_ISFILE: u64 = 9;
pub const SYS_READ_FILE: u64 = 10;
pub const SYS_READDIR: u64 = 11;
pub const SYS_MKDIR: u64 = 12;
pub const SYS_TOUCH: u64 = 13;
pub const SYS_REMOVE: u64 = 14;
pub const SYS_WRITE_FILE: u64 = 15;
pub const SYS_ABI_VERSION: u64 = 16;
pub const SYS_OPEN: u64 = 17;
pub const SYS_READ_FD: u64 = 18;
pub const SYS_WRITE_FD: u64 = 19;
pub const SYS_CLOSE: u64 = 20;
pub const SYS_DISK_COUNT: u64 = 21;
pub const SYS_DISK_SECTORS: u64 = 22;
pub const SYS_INSTALL_DISK: u64 = 23;
pub const SYS_STAT: u64 = 24;
pub const SYS_DMESG: u64 = 25;
pub const SYS_SHUTDOWN_REQUEST: u64 = 26;
pub const SYS_SHUTDOWN_POLL: u64 = 27;
pub const SYS_POWER_OFF: u64 = 28;
pub const SYS_KILL: u64 = 29;
pub const SYS_SIGACTION: u64 = 30;
pub const SYS_SIGRETURN: u64 = 31;
pub const SYS_PROCESS_LIST: u64 = 32;
pub const SYS_AUTHENTICATE: u64 = 33;
pub const SYS_GETIDENTITY: u64 = 34;
pub const SYS_INSTALL_DISK_CONFIG: u64 = 35;
pub const SYS_SBRK: u64 = 36;
pub const SYS_LSEEK: u64 = 37;
pub const SYS_FSTAT: u64 = 38;
pub const SYS_GETTIMEOFDAY: u64 = 39;
// Process and IPC primitives added for POSIX-style shells and pipelines.
pub const SYS_DUP: u64 = 40;
pub const SYS_DUP2: u64 = 41;
pub const SYS_PIPE: u64 = 42;
pub const SYS_SPAWN: u64 = 43;
pub const SYS_WAIT: u64 = 44;
/// `fork(2)`: duplica o processo atual. Devolve 0 no filho, o pid no pai.
pub const SYS_FORK: u64 = 45;
/// `waitpid(pid, &status, flags)`: espera um filho e coleta o status.
pub const SYS_WAITPID: u64 = 46;
/// `execve(path, argv, envp)`: substitui a imagem do processo atual.
pub const SYS_EXECVE: u64 = 47;

pub const SIGNAL_HUP: u64 = 1;
pub const SIGNAL_INT: u64 = 2;
pub const SIGNAL_KILL: u64 = 9;
pub const SIGNAL_SEGV: u64 = 11;
pub const SIGNAL_TERM: u64 = 15;
pub const SIGNAL_CONT: u64 = 18;
pub const SIGNAL_STOP: u64 = 19;
pub const SIGNAL_DEFAULT: u64 = 0;
pub const SIGNAL_IGNORE: u64 = 1;

pub const PROCESS_RUNNING: u32 = 1;
pub const PROCESS_STOPPED: u32 = 2;
// Large enough for a shell plus a handful of pipeline stages. Each slot costs a
// 64 KiB kernel stack (see `KERNEL_STACKS`), so this is not free memory.
pub const MAX_PROCESSES: usize = 16;
pub const PROCESS_NAME_SIZE: usize = 64;
pub const ACCOUNT_NAME_SIZE: usize = 32;
pub const ACCOUNT_PASSWORD_SIZE: usize = 64;
pub const HOSTNAME_SIZE: usize = 64;

pub const FD_STDIN: u64 = 0;
pub const FD_STDOUT: u64 = 1;
pub const FD_STDERR: u64 = 2;
pub const MAX_OPEN_FDS: usize = 24;

pub const OPEN_READ: u64 = 1 << 0;
pub const OPEN_WRITE: u64 = 1 << 1;
pub const OPEN_CREATE: u64 = 1 << 2;
pub const OPEN_TRUNCATE: u64 = 1 << 3;
pub const OPEN_APPEND: u64 = 1 << 4;
pub const OPEN_EXCLUSIVE: u64 = 1 << 5;

pub const MAX_EXEC_ARGS: usize = 12;
pub const MAX_EXEC_ENV: usize = 12;
pub const MAX_EXEC_ITEM_SIZE: usize = 128;

/// Number of pipes available system-wide (each has a read end and a write end).
pub const MAX_PIPES: usize = 8;
/// Capacity of each pipe ring buffer, in bytes.
pub const PIPE_CAPACITY: usize = 4096;
pub const MAX_CONSOLE_WRITE: usize = 512;
pub const MAX_PATH_QUERY: usize = 128;
pub const MAX_MUTABLE_PATH: usize = 256;
pub const MAX_READ_BUFFER: usize = 4096;
pub const MAX_WRITE_FILE_SIZE: usize = 4096;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UserArg {
    pub address: u64,
    pub length: u64,
}

/// Describes how a child created by `SYS_SPAWN` should see its standard
/// descriptors. Each field is a descriptor number owned by the *caller*: the
/// kernel duplicates it into the child, so `0`, `1`, `2` keep meaning
/// stdin/stdout/stderr. `-1` means "leave it as the default".
///
/// This is how a shell sets up a pipeline (`child 1 | child 2`) without a real
/// `fork`: give the left child `stdout = pipe_write`, the right child
/// `stdin = pipe_read`, and close the unused ends in the parent afterwards.
/// Describes how a child created by `SYS_SPAWN` should see its standard
/// descriptors, and carries the environment for the child.
///
/// The six syscall registers are all used by `SYS_SPAWN`, so the environment
/// (pointer and count) travels inside this structure instead of taking extra
/// registers. Both fields are `u64` so the layout is identical for the kernel
/// and for 64-bit userspace.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpawnIo {
    pub stdin_fd: i64,
    pub stdout_fd: i64,
    pub stderr_fd: i64,
    /// Ponteiro para um array de [`UserArg`] com o ambiente do filho.
    pub environment: u64,
    /// Quantidade de entradas em `environment`.
    pub environment_count: u64,
}

impl SpawnIo {
    /// Sentinel meaning "keep the default descriptor".
    pub const INHERIT: i64 = -1;
}

/// `Default` inherits every descriptor and passes no environment.
///
/// A caller that only wants to override one descriptor writes just that field:
///
/// ```ignore
/// SpawnIo { stdout_fd: write_end as i64, ..SpawnIo::default() }
/// ```
impl Default for SpawnIo {
    fn default() -> Self {
        Self {
            stdin_fd: Self::INHERIT,
            stdout_fd: Self::INHERIT,
            stderr_fd: Self::INHERIT,
            environment: 0,
            environment_count: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UserStat {
    pub size: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u64,
    pub state: u32,
    pub pending_signals: u32,
    pub name: [u8; PROCESS_NAME_SIZE],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstallConfig {
    pub username_length: u8,
    pub password_length: u8,
    pub hostname_length: u8,
    pub add_user: u8,
    pub administrator: u8,
    pub reserved: [u8; 3],
    pub username: [u8; ACCOUNT_NAME_SIZE],
    pub password: [u8; ACCOUNT_PASSWORD_SIZE],
    pub hostname: [u8; HOSTNAME_SIZE],
}

impl Default for InstallConfig {
    fn default() -> Self {
        Self {
            username_length: 0,
            password_length: 0,
            hostname_length: 0,
            add_user: 0,
            administrator: 0,
            reserved: [0; 3],
            username: [0; ACCOUNT_NAME_SIZE],
            password: [0; ACCOUNT_PASSWORD_SIZE],
            hostname: [0; HOSTNAME_SIZE],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserIdentity {
    pub uid: u32,
    pub gid: u32,
    pub is_admin: u32,
    pub username_length: u32,
    pub username: [u8; ACCOUNT_NAME_SIZE],
    pub hostname_length: u32,
    pub hostname: [u8; HOSTNAME_SIZE],
}

impl Default for UserIdentity {
    fn default() -> Self {
        Self {
            uid: 0,
            gid: 0,
            is_admin: 0,
            username_length: 0,
            username: [0; ACCOUNT_NAME_SIZE],
            hostname_length: 0,
            hostname: [0; HOSTNAME_SIZE],
        }
    }
}

impl Default for ProcessInfo {
    fn default() -> Self {
        Self {
            pid: 0,
            state: 0,
            pending_signals: 0,
            name: [0; PROCESS_NAME_SIZE],
        }
    }
}

impl UserArg {
    pub const EMPTY: Self = Self {
        address: 0,
        length: 0,
    };
}
