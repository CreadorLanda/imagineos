//! Isola `fork` + `waitpid` sem `execve`, para verificar o escalonamento.
//!
//! Cada passo imprime uma linha, de modo que o ponto exato de bloqueio fica
//! visível na serial. Uso: `/bin/forktest`.

#![no_std]
#![no_main]

mod common;

#[unsafe(no_mangle)]
extern "C" fn _start(
    _argc: usize,
    _argv: *const *const u8,
    _envc: usize,
    _envp: *const *const u8,
) -> ! {
    common::write(b"forktest: inicio\n");

    match imagineos::process::fork() {
        Ok(0) => {
            // --- Filho ---
            common::write(b"forktest: [filho] rodando\n");
            // Dá chance ao pai de ser escalonado de novo antes de sair.
            let _ = imagineos::process::yield_now();
            common::write(b"forktest: [filho] saindo com 42\n");
            common::exit(42);
        }
        Ok(pid) => {
            common::write(b"forktest: [pai] fork devolveu pid=");
            write_number(pid);
            common::write(b"\nforktest: [pai] chamando waitpid\n");
            match imagineos::process::waitpid(pid) {
                Ok((child, status)) => {
                    common::write(b"forktest: [pai] waitpid devolveu pid=");
                    write_number(child as i64);
                    common::write(b" status=");
                    write_number(i64::from(imagineos::process::exit_code(status)));
                    common::write(b"\n");
                }
                Err(error) => {
                    common::write(b"forktest: [pai] waitpid falhou errno=");
                    write_number(error.code() as i64);
                    common::write(b"\n");
                }
            }
        }
        Err(error) => {
            common::write(b"forktest: fork falhou errno=");
            write_number(error.code() as i64);
            common::write(b"\n");
        }
    }

    common::write(b"forktest: fim\n");
    common::exit(0)
}

fn write_number(value: i64) {
    let mut digits = [0u8; 20];
    let mut cursor = digits.len();
    let mut magnitude = value.unsigned_abs();
    if magnitude == 0 {
        common::write(b"0");
        return;
    }
    while magnitude != 0 {
        cursor -= 1;
        digits[cursor] = b'0' + (magnitude % 10) as u8;
        magnitude /= 10;
    }
    if value < 0 {
        common::write(b"-");
    }
    common::write(&digits[cursor..]);
}
