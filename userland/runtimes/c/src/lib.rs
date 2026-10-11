/*
    Runtime de programas em C do ImagineOS.

    Esta crate implementa, em Rust, os componentes de runtime que antes
    viviam em `userland/libc/` (crt0.S, crt0.c, crt_empty.S, signal.S e
    setjmp.S). Ela produz os mesmos símbolos com nomes C para que a libc em
    C e o TinyCC possam linkar contra ela sem alterações.

    O único trecho que permanece em assembly é o ponto de entrada `_start`,
    pois executa antes de qualquer código Rust poder rodar. Ele é emitido
    via `global_asm!`, ainda dentro desta crate Rust.

    userland/runtimes/c/src/lib.rs
*/

#![no_std]

use core::arch::global_asm;
use core::panic::PanicInfo;

/// Trata panics: sem unwinding disponível, o processo é encerrado com um
/// código distinto (127) para que falhas fiquem visíveis ao shell.
#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    imagineos::process::exit(127)
}

// Ponto de entrada dos programas em C.
//
// O kernel entra no processo com a seguinte convenção de registradores:
//   rdi = argc, rsi = argv, rdx = envc, rcx = envp, rsp = stack do usuário
//
// O `_start` apenas normaliza o frame (`rbp` zerado e `rsp` alinhado a 16
// bytes, como exige a ABI SysV) e salta para `c_runtime_init`, que recebe
// exatamente esses quatro registradores de argumento.
//
// A nota `.note.GNU-stack` substitui o antigo `crt_empty.S` e mantém o
// binário compatível com linkers que exigem a seção de stack não-executável.
global_asm!(
    r#"
    .section .note.GNU-stack,"",@progbits

    .text
    .global _start
    .type _start, @function
_start:
    xorq %rbp, %rbp
    andq $-16, %rsp
    call c_runtime_init
    ud2
    .size _start, .-_start
"#,
    options(att_syntax)
);

unsafe extern "C" {
    fn main(argc: i32, argv: *mut *mut u8) -> i32;
    /// Vetor de ambiente global definido pela libc em C (`stdlib.c`).
    static mut environ: *mut *mut u8;
}

/// Inicializa o runtime C e chama `main`.
///
/// Verifica a versão da ABI antes de qualquer trabalho; se o kernel expõe uma
/// ABI diferente da esperada, o processo termina com código 2, evitando
/// corromper o estado do sistema com chamadas incompatíveis.
///
/// # Safety
/// Deve ser chamada apenas pelo `_start`, com os vetores `argv`/`envp`
/// fornecidos pelo kernel e válidos durante toda a execução.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn c_runtime_init(
    argc: i32,
    argv: *mut *mut u8,
    _envc: i32,
    envp: *mut *mut u8,
) -> ! {
    if imagineos::syscall::abi_version() != Ok(imagineos::abi::ABI_VERSION) {
        c_exit(2);
    }
    environ = envp;
    c_exit(main(argc, argv));
}

/// Termina o processo com o código informado.
///
/// Mantida separada de `exit` (definida na libc) porque o TinyCC fornece a
/// sua própria `exit` no modo `tcc -run`, e queremos um caminho interno que
/// não dependa desse símbolo.
fn c_exit(status: i32) -> ! {
    imagineos::process::exit(status)
}

// Restaurador de contexto de sinais.
//
// O kernel invoca este endereço ao retornar de um handler de sinal. Ele
// executa a syscall de restauração (número 31) e encerra a thread caso a
// syscall retorne, pois o contexto já deve ter sido retomado.
global_asm!(
    r#"
    .text
    .global dc_signal_restorer
    .type dc_signal_restorer, @function
dc_signal_restorer:
    movq $31, %rax
    int $0x80
    ud2
    .size dc_signal_restorer, .-dc_signal_restorer
"#,
    options(att_syntax)
);

// `setjmp`/`longjmp` da ABI SysV x86_64.
//
// O `jmp_buf` da libc tem espaço para oito registradores de 64 bits. São
// salvos `rbx`, `rbp`, `r12`-`r15` (callee-saved), o `rsp` após o retorno e
// o endereço de retorno. `longjmp` restaura o frame e salta de volta,
// garantindo que o valor de retorno nunca seja 0.
global_asm!(
    r#"
    .text
    .global setjmp
    .type setjmp, @function
setjmp:
    movq %rbx, 0(%rdi)
    movq %rbp, 8(%rdi)
    movq %r12, 16(%rdi)
    movq %r13, 24(%rdi)
    movq %r14, 32(%rdi)
    movq %r15, 40(%rdi)
    leaq 8(%rsp), %rax
    movq %rax, 48(%rdi)
    movq (%rsp), %rax
    movq %rax, 56(%rdi)
    xorl %eax, %eax
    ret
    .size setjmp, .-setjmp

    .global longjmp
    .type longjmp, @function
longjmp:
    movl %esi, %eax
    testl %eax, %eax
    jne 1f
    movl $1, %eax
1:
    movq 0(%rdi), %rbx
    movq 8(%rdi), %rbp
    movq 16(%rdi), %r12
    movq 24(%rdi), %r13
    movq 32(%rdi), %r14
    movq 40(%rdi), %r15
    movq 56(%rdi), %rdx
    movq 48(%rdi), %rsp
    jmp *%rdx
    .size longjmp, .-longjmp
"#,
    options(att_syntax)
);