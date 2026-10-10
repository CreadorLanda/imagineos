//! Harness de testes dos módulos puros do shell (`no_std`, sem a crate do
//! kernel). Estes módulos não dependem de syscalls, então rodam no host:
//!
//! ```sh
//! rustup run stable rustc --edition 2021 --test userland/apps/shell/tests.rs -o /tmp/shell-tests
//! /tmp/shell-tests
//! ```
//!
//! `exec.rs` não entra aqui: ele chama syscalls e só roda dentro do sistema.

#[path = "uri.rs"]
mod uri;
#[path = "lexer.rs"]
mod lexer;
#[path = "env.rs"]
mod env;
#[path = "expand.rs"]
mod expand;
#[path = "parser.rs"]
mod parser;
