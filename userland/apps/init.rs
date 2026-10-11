//! init — PID 1, em duas fases, como um initramfs Linux.
//!
//! **Fase 1 (initramfs):** monta o disco, prepara a raiz real (DFS) e, se ela
//! existir, troca a raiz com `pivot_root` e re-executa `/sbin/init` — agora o
//! init **da raiz real** (fase 2). Nesta fase o console continua com o desenho
//! do kernel (não há ainda um servidor de display).
//!
//! **Fase 2 (raiz real):** supervisiona getty/shell e o servidor de display
//! opcional, exatamente como um init faz após o boot completo.
//!
//! Um init POSIX não deve encerrar nunca; qualquer falha de re-exec cai para a
//! supervisão normal.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

#[unsafe(no_mangle)]
extern "C" fn _start(
    _argc: usize,
    _argv: *const *const u8,
    _envc: usize,
    _envp: *const *const u8,
) -> ! {
    // ------------------------------------------------------------------
    // Fase 1: pivota para a raiz real, se ela estiver preparada.
    // ------------------------------------------------------------------
    if !phase_two_started() {
        if imagineos::fs::is_file("/sbin/init").unwrap_or(false) {
            let _ = imagineos::console::write_all(b"init: pivoting root to real root\n");
            // `put_old` é onde o initramfs continua montado (equivalente ao
            // /initrd do Linux). A memória do initramfs não é liberada neste
            // kernel (não há reclaim de frames), mas deixa de ser a raiz.
            match imagineos::process::pivot_root("/", "/initrd") {
                Ok(()) => {
                    let _ = imagineos::console::write_all(b"init: root pivoted; re-exec from real root\n");
                    // Re-executa o init da raiz real no mesmo PID 1. O `execve`
                    // substitui a imagem e o marcador de fase já vem setado pelo
                    // kernel (o pivô só acontece uma vez por boot).
                    match imagineos::process::exec("/sbin/init", &["/sbin/init"], &[]) {
                        Ok(_) => loop {
                            let _ = imagineos::process::yield_now();
                        },
                        Err(_) => {
                            let _ = imagineos::console::write_all(b"init: re-exec failed; continuing from initramfs\n");
                        }
                    }
                }
                Err(_) => {
                    let _ = imagineos::console::write_all(b"init: pivot_root failed; continuing from initramfs\n");
                }
            }
        } else {
            let _ = imagineos::console::write_all(b"init: no /sbin/init on real root; staying on initramfs\n");
        }
    }
    run_supervisor()
}

/// Detecta se este init já é o da raiz real (fase 2).
///
/// O critério é o mesmo que o kernel expõe via VFS: a raiz global é o DFS
/// depois de um `pivot_root` bem-sucedido. Para o userspace isso significa que
/// `/sbin/init` desta raiz deve conter o marcador de fase; usamos um arquivo
/// sentinel criado pelo próprio kernel no pivô (`/run/initrd/pivoted`), no
/// espírito do `/run` do FHS.
fn phase_two_started() -> bool {
    imagineos::fs::is_file("/run/pivoted").unwrap_or(false)
}

fn run_supervisor() -> ! {
    if imagineos::console::write_all(b"Astrid init: PID 1 supervising the system\n").is_err() {
        imagineos::process::exit(1);
    }
    // Supervisor de serviços: quando /etc/init contém "horust", o PID 1
    // delega a supervisão ao horust-lite (formato de serviços Horust em
    // /etc/horust/services). O horust substitui a imagem deste processo
    // (execve, como um init style-Linux que troca de init).
    let mut init_choice = [0u8; 32];
    if let Ok(used) = imagineos::fs::read_file("/etc/init", &mut init_choice) {
        if core::str::from_utf8(&init_choice[..used]).unwrap_or("")
            .trim()
            .contains("horust")
        {
            let _ = imagineos::console::write_all(b"init: delegating supervision to horust\n");
            match imagineos::process::exec("/sbin/horust", &["/sbin/horust"], &[]) {
                Ok(_pid) => loop {
                    let _ = imagineos::process::yield_now();
                },
                Err(_) => {
                    let _ = imagineos::console::write_all(b"init: horust missing; falling back to getty supervision\n");
                }
            }
        }
    }
    // O display server é opcional: crie/remova /etc/display.on no RAMFS para
    // habilitar/desabilitar o boot gráfico durante bring-up. Quando termina (q
    // ou crash), o init volta ao getty de texto.
    if imagineos::fs::is_file("/etc/display.on").unwrap_or(false) {
        let _ = imagineos::console::write_all(b"init: starting display server\n");
        if let Ok(pid) = imagineos::process::exec("/bin/display", &["/bin/display"], &[]) {
            // O painel EDE é um cliente separado do WM (protocolo de
            // superfícies); começa junto, em paralelo com o display server.
            let _ = imagineos::process::spawn(
                "/bin/ede-panel",
                &["/bin/ede-panel"],
                &[],
                imagineos::abi::SpawnIo::default(),
            );
            let _ = imagineos::process::waitpid(pid as i64);
            let _ = imagineos::console::write_all(b"init: display server exited; returning to getty\n");
        }
    }
    let mut getty_pid = 0;
    loop {
        if getty_pid == 0 || !process_is_active(getty_pid) {
            getty_pid = match imagineos::process::exec("/sbin/getty", &["/sbin/getty"], &[]) {
                Ok(pid) => pid,
                Err(_) => {
                    let _ = imagineos::console::write_all(b"init: unable to start getty\n");
                    let _ = imagineos::process::yield_now();
                    continue;
                }
            };
        }
        match imagineos::shutdown::take_request() {
            Ok(true) => {
                if imagineos::console::write_all(
                    b"init: shutdown requested; syncing storage and powering off\n",
                )
                .is_err()
                {
                    imagineos::process::exit(1);
                }
                if imagineos::shutdown::power_off().is_err() {
                    let _ = imagineos::console::write_all(
                        b"init: shutdown failed; system remains running\n",
                    );
                }
            }
            Ok(false) => {}
            Err(_) => {
                let _ = imagineos::console::write_all(b"init: shutdown IPC failed\n");
            }
        }
        if imagineos::process::yield_now().is_err() {
            imagineos::process::exit(1);
        }
    }
}

fn process_is_active(pid: usize) -> bool {
    let mut processes = [imagineos::abi::ProcessInfo::default(); imagineos::abi::MAX_PROCESSES];
    imagineos::process::list(&mut processes)
        .ok()
        .is_some_and(|count| {
            processes
                .iter()
                .take(count)
                .any(|process| process.pid == pid as u64)
        })
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    imagineos::process::exit(127)
}
