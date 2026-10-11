//! Shell POSIX do ImagineOS.
//!
//! Sintaxe suportada (subconjunto fiel, com as limitações documentadas):
//!
//! * listas: `;`, `&&`, `||`, `&` (background);
//! * pipelines: `a | b | c` (até `MAX_PIPELINE` estágios);
//! * redirecionamento: `>`, `>>`, `<`;
//! * quoting: `'literal'`, `"com $VAR"`, escapes com `\`;
//! * expansão: `$VAR`, `${VAR}`, `$?`, `$$`;
//! * builtins: `cd`, `pwd`, `echo`, `export`, `unset`, `set`, `exit`, `true`,
//!   `false`, `:`, `help`.
//!
//! Limitações conhecidas (exigem mais kernel): sem `$(...)`, sem subshell
//! `( )`, sem substituição de processo, sem glob (`*`), sem funções, sem
//! `if`/`for`/`while` e sem status por estágio de pipeline (`waitpid`).

#![no_std]
#![no_main]

mod env;
mod exec;
mod expand;
mod lexer;
mod parser;
mod uri;

use core::panic::PanicInfo;

use env::Environment;
use exec::{Executor, STATUS_FAILURE};
use lexer::{tokenize, LexError, Token, Word, MAX_TOKENS};
use parser::{parse_list, Connector, Entry, ParseError, MAX_ENTRIES};

/// Comprimento máximo de uma linha de comando digitada.
const LINE_SIZE: usize = 256;

#[unsafe(no_mangle)]
extern "C" fn _start(
    argc: usize,
    argv: *const *const u8,
    _envc: usize,
    _envp: *const *const u8,
) -> ! {
    let mut environment = Environment::new();
    initialise_from_identity(&mut environment);

    let mut cwd = [0u8; uri::MAX_PATH];
    cwd[0] = b'/';
    let mut cwd_length = 1usize;

    // `shell -c "cmd"` executa um comando e sai; `shell script` executa as
    // linhas de um arquivo. Sem argumentos, entra no modo interativo.
    //
    // `argv[0]` é o nome do próprio programa e não conta como argumento; usar
    // `[script]` contra a fatia completa nunca casaria porque ela sempre tem
    // pelo menos um elemento.
    let arguments = read_arguments(argc, argv);
    let operands = &arguments[1..operand_count(argc)];
    match operands {
        [flag, command] if *flag == "-c" => {
            let status = run_line(
                command.as_bytes(),
                &mut environment,
                &mut cwd,
                &mut cwd_length,
            );
            imagineos::process::exit(if status == EXIT_REQUESTED { 0 } else { status });
        }
        [script] => {
            let status = run_script(script, &mut environment, &mut cwd, &mut cwd_length);
            imagineos::process::exit(status);
        }
        _ => {}
    }

    let mut line = [0u8; LINE_SIZE];
    let mut length = 0usize;

    prompt(&environment, &cwd[..cwd_length]);
    loop {
        let character = match imagineos::console::read_char() {
            Ok(character) => character,
            Err(_) => imagineos::process::exit(1),
        };
        match character {
            '\r' | '\n' => {
                write(b"\n");
                let status = run_line(
                    &line[..length],
                    &mut environment,
                    &mut cwd,
                    &mut cwd_length,
                );
                line[..length].fill(0);
                length = 0;
                if status == EXIT_REQUESTED {
                    imagineos::process::exit(0);
                }
                if status != 0 {
                    // `status` carrega o código de saída pedido por `exit N`.
                    let code = environment
                        .get(b"?")
                        .and_then(|value| core::str::from_utf8(value).ok())
                        .and_then(|text| text.parse::<i32>().ok())
                        .unwrap_or(status);
                    let _ = code;
                }
                prompt(&environment, &cwd[..cwd_length]);
            }
            '\u{8}' | '\u{7f}' => {
                if length > 0 {
                    length -= 1;
                    // Apaga um caractere UTF-8 inteiro.
                    while length > 0 && line[length] & 0xc0 == 0x80 {
                        length -= 1;
                    }
                    write(b"\x08 \x08");
                }
            }
            value if !value.is_control() && !is_keyboard_event(value) => {
                let mut encoded = [0u8; 4];
                let bytes = value.encode_utf8(&mut encoded).as_bytes();
                if length + bytes.len() <= line.len() {
                    line[length..length + bytes.len()].copy_from_slice(bytes);
                    length += bytes.len();
                    write(bytes);
                }
            }
            _ => {}
        }
        if imagineos::process::yield_now().is_err() {
            imagineos::process::exit(1);
        }
    }
}

/// Sentinela devolvida quando `exit` foi executado.
const EXIT_REQUESTED: i32 = i32::MIN;

/// Quantos dos argumentos capturados são válidos (limitado pelo buffer).
fn operand_count(argc: usize) -> usize {
    argc.min(MAX_ARGUMENTS)
}

/// Argumentos máximos lidos: `argv[0]`, mais `-c` e o comando.
const MAX_ARGUMENTS: usize = 3;

/// Coleta os argumentos do processo em `&str`.
fn read_arguments(argc: usize, argv: *const *const u8) -> [&'static str; MAX_ARGUMENTS] {
    let mut arguments = [""; MAX_ARGUMENTS];
    if argv.is_null() {
        return arguments;
    }
    for index in 0..argc.min(MAX_ARGUMENTS) {
        let pointer = unsafe { *argv.add(index) };
        if pointer.is_null() {
            continue;
        }
        // Os argumentos vêm do kernel como strings NUL-terminadas na pilha.
        let mut length = 0usize;
        while unsafe { *pointer.add(length) } != 0 && length < 512 {
            length += 1;
        }
        let bytes = unsafe { core::slice::from_raw_parts(pointer, length) };
        if let Ok(text) = core::str::from_utf8(bytes) {
            arguments[index] = text;
        }
    }
    arguments
}

/// Executa cada linha de um arquivo de script, numa sub-rotina separada para
/// que `exit` dentro do script encerre apenas a execução do arquivo.
fn run_script(
    path: &str,
    environment: &mut Environment,
    cwd: &mut [u8; uri::MAX_PATH],
    cwd_length: &mut usize,
) -> i32 {
    let mut buffer = [0u8; 4096];
    let length = match imagineos::fs::read_file(path, &mut buffer) {
        Ok(length) => length,
        Err(_) => {
            write(b"shell: nao foi possivel ler o script\n");
            return 1;
        }
    };
    let mut status = 0;
    for line in buffer[..length].split(|byte| *byte == b'\n') {
        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
            continue;
        }
        status = run_line(line, environment, cwd, cwd_length);
        if status == EXIT_REQUESTED {
            // Um `exit` num script devolve o status acumulado, não encerra o
            // shell chamador.
            return 0;
        }
    }
    status
}

/// Tokeniza, parseia e executa uma linha. Devolve o status ou `EXIT_REQUESTED`.
fn run_line(
    line: &[u8],
    environment: &mut Environment,
    cwd: &mut [u8; uri::MAX_PATH],
    cwd_length: &mut usize,
) -> i32 {
    if line.iter().all(|byte| byte.is_ascii_whitespace()) {
        return 0;
    }

    let mut tokens = [Token::Word(Word::EMPTY); MAX_TOKENS];
    let count = match tokenize(line, &mut tokens) {
        Ok(0) => return 0,
        Ok(count) => count,
        Err(error) => {
            report_lex_error(error);
            return STATUS_FAILURE;
        }
    };

    let mut entries = [Entry {
        pipeline: parser::Pipeline::EMPTY,
        connector: Connector::End,
    }; MAX_ENTRIES];
    let entry_count = match parse_list(&tokens[..count], &mut entries) {
        Ok(count) => count,
        Err(error) => {
            report_parse_error(error);
            return STATUS_FAILURE;
        }
    };

    let mut executor = Executor::new(environment, cwd, cwd_length);
    let status = executor.run_list(&entries[..entry_count]);
    if executor.should_exit {
        return EXIT_REQUESTED;
    }
    status
}

fn initialise_from_identity(environment: &mut Environment) {
    let Ok(identity) = imagineos::users::identity() else {
        return;
    };
    let username = &identity.username[..identity.username_length as usize];
    let hostname = &identity.hostname[..identity.hostname_length as usize];
    let _ = environment.set(b"USER", username, true);
    let _ = environment.set(b"HOSTNAME", hostname, true);
    if identity.uid != 0 {
        let mut home = [0u8; env::VALUE_SIZE];
        home[..6].copy_from_slice(b"/home/");
        let length = 6 + username.len();
        if length <= home.len() {
            home[6..length].copy_from_slice(username);
            let _ = environment.set(b"HOME", &home[..length], true);
        }
    }
}

fn prompt(environment: &Environment, cwd: &[u8]) {
    // Copia os textos para buffers locais: `identity` é temporário e os campos
    // não sobrevivem ao `match`.
    let mut username_buffer = [0u8; 64];
    let mut hostname_buffer = [0u8; 64];
    let mut username_length = 4usize;
    let mut hostname_length = 9usize;
    username_buffer[..4].copy_from_slice(b"root");
    hostname_buffer[..9].copy_from_slice(b"imagineos");
    let mut is_root = true;
    if let Ok(identity) = imagineos::users::identity() {
        username_length = identity.username_length as usize;
        hostname_length = identity.hostname_length as usize;
        username_buffer[..username_length].copy_from_slice(&identity.username[..username_length]);
        hostname_buffer[..hostname_length].copy_from_slice(&identity.hostname[..hostname_length]);
        is_root = identity.uid == 0;
    }
    let username = core::str::from_utf8(&username_buffer[..username_length]).unwrap_or("root");
    let hostname = core::str::from_utf8(&hostname_buffer[..hostname_length]).unwrap_or("imagineos");
    let _ = environment;
    write(b"\x1b[32m");
    write(username.as_bytes());
    write(b"\x1b[0m@\x1b[36m");
    write(hostname.as_bytes());
    write(b"\x1b[0m:\x1b[34m");
    write(cwd);
    if is_root {
        write(b"\x1b[0m# ");
    } else {
        write(b"\x1b[0m$ ");
    }
}

fn report_lex_error(error: LexError) {
    let message: &[u8] = match error {
        LexError::UnterminatedQuote => b"shell: aspas nao fechadas\n",
        LexError::TooManyTokens => b"shell: linha com tokens demais\n",
        LexError::WordTooLong => b"shell: palavra longa demais\n",
    };
    write(message);
}

fn report_parse_error(error: ParseError) {
    let message: &[u8] = match error {
        ParseError::UnexpectedOperator => b"shell: operador inesperado\n",
        ParseError::PipelineTooLong => b"shell: pipeline com estagios demais\n",
        ParseError::TooManyWords => b"shell: argumentos demais\n",
        ParseError::MissingRedirectionTarget => b"shell: redirecionamento sem destino\n",
    };
    write(message);
}

fn is_keyboard_event(character: char) -> bool {
    ('\u{f0000}'..='\u{f0007}').contains(&character)
}

fn write(bytes: &[u8]) {
    if imagineos::console::write_all(bytes).is_err() {
        imagineos::process::exit(1);
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    imagineos::process::exit(127)
}
