use imagineos_abi::UserArg;

use crate::{checked, path_bytes, syscall, Error, Result};

pub fn pid() -> Result<usize> {
    let value = checked(syscall::invoke(crate::abi::SYS_GETPID, [0; 6]))?;
    usize::try_from(value).map_err(|_| Error::INVALID_ARGUMENT)
}

pub fn yield_now() -> Result<()> {
    checked(syscall::invoke(crate::abi::SYS_YIELD, [0; 6])).map(|_| ())
}

pub fn list(output: &mut [crate::abi::ProcessInfo; crate::abi::MAX_PROCESSES]) -> Result<usize> {
    let count = checked(syscall::invoke(
        crate::abi::SYS_PROCESS_LIST,
        [output.as_mut_ptr() as u64, output.len() as u64, 0, 0, 0, 0],
    ))?;
    usize::try_from(count).map_err(|_| Error::INVALID_ARGUMENT)
}

pub fn exec(path: &str, arguments: &[&str], environment: &[&str]) -> Result<usize> {
    let path = path_bytes(path, crate::abi::MAX_EXEC_ITEM_SIZE)?;
    if arguments.len() > crate::abi::MAX_EXEC_ARGS || environment.len() > crate::abi::MAX_EXEC_ENV {
        return Err(Error::TOO_BIG);
    }

    let mut argument_records = [UserArg::default(); crate::abi::MAX_EXEC_ARGS];
    for (record, value) in argument_records.iter_mut().zip(arguments) {
        *record = user_arg(value)?;
    }
    let mut environment_records = [UserArg::default(); crate::abi::MAX_EXEC_ENV];
    for (record, value) in environment_records.iter_mut().zip(environment) {
        *record = user_arg(value)?;
    }

    let result = checked(syscall::invoke(
        crate::abi::SYS_EXEC,
        [
            path.as_ptr() as u64,
            path.len() as u64,
            argument_records.as_ptr() as u64,
            arguments.len() as u64,
            environment_records.as_ptr() as u64,
            environment.len() as u64,
        ],
    ))?;
    usize::try_from(result).map_err(|_| Error::INVALID_ARGUMENT)
}

/// Creates a child process whose standard descriptors are chosen by the caller.
///
/// `io` names descriptors the caller already owns; the kernel duplicates them
/// into the child, so this is the building block for pipelines:
///
/// ```ignore
/// let (read_end, write_end) = imagineos::process::pipe()?;
/// let left = imagineos::process::spawn("/bin/ls", &["ls"], &[], SpawnIo {
///     stdout_fd: write_end as i64, ..SpawnIo::default()
/// })?;
/// imagineos::process::close(write_end)?; // let the reader see EOF
/// ```
pub fn spawn(
    path: &str,
    arguments: &[&str],
    environment: &[&str],
    io: crate::abi::SpawnIo,
) -> Result<usize> {
    let path = path_bytes(path, crate::abi::MAX_EXEC_ITEM_SIZE)?;
    if arguments.len() > crate::abi::MAX_EXEC_ARGS || environment.len() > crate::abi::MAX_EXEC_ENV {
        return Err(Error::TOO_BIG);
    }

    let mut argument_records = [UserArg::default(); crate::abi::MAX_EXEC_ARGS];
    for (record, value) in argument_records.iter_mut().zip(arguments) {
        *record = user_arg(value)?;
    }
    let mut environment_records = [UserArg::default(); crate::abi::MAX_EXEC_ENV];
    for (record, value) in environment_records.iter_mut().zip(environment) {
        *record = user_arg(value)?;
    }

    // O ambiente vai dentro de `SpawnIo`: os seis registradores são todos
    // consumidos pelos outros parâmetros.
    let io = crate::abi::SpawnIo {
        environment: environment_records.as_ptr() as u64,
        environment_count: environment.len() as u64,
        ..io
    };

    let result = checked(syscall::invoke(
        crate::abi::SYS_SPAWN,
        [
            path.as_ptr() as u64,
            path.len() as u64,
            argument_records.as_ptr() as u64,
            arguments.len() as u64,
            (&io as *const crate::abi::SpawnIo) as u64,
            0,
        ],
    ))?;
    usize::try_from(result).map_err(|_| Error::INVALID_ARGUMENT)
}

/// `execve(2)`: substitui a imagem do processo atual pelo programa em `path`.
///
/// Não retorna em caso de sucesso — o processo recomeça na entrada do novo
/// programa, com os mesmos descritores. É o par natural de [`fork`]:
///
/// ```ignore
/// match imagineos::process::fork()? {
///     0 => imagineos::process::execve("/bin/ls", &["ls"], &[])?,
///     child => imagineos::process::waitpid(child as i64)?,
/// }
/// ```
pub fn execve(path: &str, arguments: &[&str], environment: &[&str]) -> Result<usize> {
    let path = path_bytes(path, crate::abi::MAX_EXEC_ITEM_SIZE)?;
    if arguments.len() > crate::abi::MAX_EXEC_ARGS || environment.len() > crate::abi::MAX_EXEC_ENV {
        return Err(Error::TOO_BIG);
    }
    let mut argument_records = [UserArg::default(); crate::abi::MAX_EXEC_ARGS];
    for (record, value) in argument_records.iter_mut().zip(arguments) {
        *record = user_arg(value)?;
    }
    let mut environment_records = [UserArg::default(); crate::abi::MAX_EXEC_ENV];
    for (record, value) in environment_records.iter_mut().zip(environment) {
        *record = user_arg(value)?;
    }
    let result = checked(syscall::invoke(
        crate::abi::SYS_EXECVE,
        [
            path.as_ptr() as u64,
            path.len() as u64,
            argument_records.as_ptr() as u64,
            arguments.len() as u64,
            environment_records.as_ptr() as u64,
            environment.len() as u64,
        ],
    ))?;
    // Só chega aqui em caso de erro devolvido pelo kernel.
    usize::try_from(result).map_err(|_| Error::INVALID_ARGUMENT)
}

/// `fork(2)`: duplica o processo atual.
///
/// Devolve `Ok(0)` no filho e `Ok(pid)` no pai. O filho é uma cópia
/// independente: memória, descritores e credenciais são duplicados.
///
/// # Por que o retorno é `i64` e não `Result<usize>`
///
/// O kernel devolve o resultado do `fork` no registrador `rax`: `0` no filho,
/// o pid no pai. A convenção do Rust para um `Result<usize>` (16 bytes) usa um
/// ponteiro de retorno alternativo, e o filho **não** pode recebê-lo de volta
/// pelo `fork` — o frame clonado não carrega esse ponteiro, e zerar `rax`
/// fazia o filho escrever o resultado no endereço nulo. Um retorno de 64 bits
/// em um único registrador evita o problema e espelha o `pid_t fork(void)` do C.
pub fn fork() -> Result<i64> {
    let result = checked(syscall::invoke(crate::abi::SYS_FORK, [0; 6]))?;
    Ok(result as i64)
}

/// `waitpid(pid, &status, 0)`: espera um filho e devolve `(pid, status)`.
///
/// `pid > 0` espera aquele filho; `pid < 0` espera qualquer filho. O `status`
/// vem no formato POSIX (código de saída nos 8 bits altos); use
/// [`exit_code`] para extraí-lo.
pub fn waitpid(pid: i64) -> Result<(usize, u32)> {
    // O kernel devolve `EAGAIN` enquanto o filho não termina; o caso é normal e
    // o chamador deve repetir (como o `waitpid` do POSIX, que bloqueia). Aqui a
    // repetição é feita dentro desta função, cedendo a CPU entre as tentativas
    // para o filho progredir.
    loop {
        let mut status: u32 = 0;
        match syscall::invoke(
            crate::abi::SYS_WAITPID,
            [pid as u64, (&mut status as *mut u32) as u64, 0, 0, 0, 0],
        ) {
            0xffff_ffff_ffff_fff5 /* -11 == EAGAIN */ => {
                if yield_now().is_err() {
                    return Err(Error::INVALID_ARGUMENT);
                }
            }
            result => {
                let result = checked(result)?;
                let child = usize::try_from(result).map_err(|_| Error::INVALID_ARGUMENT)?;
                return Ok((child, status));
            }
        }
    }
}

/// Extrai o código de saída de um `status` devolvido por [`waitpid`].
pub fn exit_code(status: u32) -> i32 {
    ((status >> 8) & 0xff) as i32
}

/// `waitpid(pid, &status, WNOHANG)`: varre filhos sem bloquear.
///
/// Devolve `Ok(None)` quando nenhum filho terminou ainda (POSIX: retorno 0),
/// `Ok(Some((pid, status)))` quando coletou um, e erro para ECHILD.
pub fn waitpid_nonblocking(pid: i64) -> Result<Option<(usize, u32)>> {
    let mut status: u32 = 0;
    let result = syscall::invoke(
        crate::abi::SYS_WAITPID,
        [pid as u64, (&mut status as *mut u32) as u64, crate::abi::WNOHANG as u64, 0, 0, 0],
    );
    if result == 0 {
        return Ok(None);
    }
    let child = usize::try_from(checked(result)?).map_err(|_| Error::INVALID_ARGUMENT)?;
    Ok(Some((child, status)))
}

/// `pivot_root(new_root, put_old)`: troca a raiz do sistema do initramfs para
/// a raiz real montada pelo chamador. Só retorna em erro.
pub fn pivot_root(new_root: &str, put_old: &str) -> Result<()> {
    let mut new_root_bytes = [0u8; crate::abi::MAX_MUTABLE_PATH];
    let mut put_old_bytes = [0u8; crate::abi::MAX_MUTABLE_PATH];
    let new_len = new_root.len();
    let old_len = put_old.len();
    if new_len >= new_root_bytes.len() || old_len >= put_old_bytes.len() {
        return Err(Error::INVALID_ARGUMENT);
    }
    new_root_bytes[..new_len].copy_from_slice(new_root.as_bytes());
    put_old_bytes[..old_len].copy_from_slice(put_old.as_bytes());
    checked(syscall::invoke(
        crate::abi::SYS_PIVOT_ROOT,
        [new_root_bytes.as_ptr() as u64, put_old_bytes.as_ptr() as u64, 0, 0, 0, 0],
    ))
    .map(|_| ())
}

/// Creates a pipe, returning `(read_end, write_end)` descriptors.
pub fn pipe() -> Result<(usize, usize)> {
    let mut endpoints = [0i32; 2];
    checked(syscall::invoke(
        crate::abi::SYS_PIPE,
        [endpoints.as_mut_ptr() as u64, 0, 0, 0, 0, 0],
    ))?;
    let read_end = usize::try_from(endpoints[0]).map_err(|_| Error::INVALID_ARGUMENT)?;
    let write_end = usize::try_from(endpoints[1]).map_err(|_| Error::INVALID_ARGUMENT)?;
    Ok((read_end, write_end))
}

/// `dup`: returns a new descriptor referring to the same object as `fd`.
pub fn duplicate(fd: usize) -> Result<usize> {
    let result = checked(syscall::invoke(
        crate::abi::SYS_DUP,
        [fd as u64, 0, 0, 0, 0, 0],
    ))?;
    usize::try_from(result).map_err(|_| Error::INVALID_ARGUMENT)
}

/// `dup2`: makes `target` refer to the same object as `source`.
pub fn duplicate_to(source: usize, target: usize) -> Result<usize> {
    let result = checked(syscall::invoke(
        crate::abi::SYS_DUP2,
        [source as u64, target as u64, 0, 0, 0, 0],
    ))?;
    usize::try_from(result).map_err(|_| Error::INVALID_ARGUMENT)
}

pub fn exit(status: i32) -> ! {
    let _ = status;
    let _ = syscall::invoke(crate::abi::SYS_EXIT, [0; 6]);
    loop {
        core::hint::spin_loop();
    }
}

fn user_arg(value: &str) -> Result<UserArg> {
    let bytes = value.as_bytes();
    if bytes.len() > crate::abi::MAX_EXEC_ITEM_SIZE {
        return Err(Error::TOO_BIG);
    }
    if bytes.contains(&0) {
        return Err(Error::INVALID_ARGUMENT);
    }
    Ok(UserArg {
        address: bytes.as_ptr() as u64,
        length: bytes.len() as u64,
    })
}
