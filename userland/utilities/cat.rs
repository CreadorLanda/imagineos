/*
    Simple implementation of the `cat` command in Rust for a no_std environment.
    (C) 2026 The Imagine Project. All rights reserved.
    ----------
    DNU Public License v1.0 (DNU-PLv1)

    This program is free software: you can redistribute it and/or modify
    it under the terms of the DNU Public License as published by
    The Imagine Project, either version 1 of the License, or (at your option) 
    any later version.
*/

#![no_std]
#![no_main]

#[allow(dead_code)]
mod common;

#[unsafe(no_mangle)]
extern "C" fn _start(
    argc: usize,
    argv: *const *const u8,
    envc: usize,
    envp: *const *const u8,
) -> ! {
    // Sem arquivos, `cat` copia o stdin para o stdout, como no POSIX. Isto é o
    // que faz `algo | cat` funcionar.
    if argc < 2 {
        let mut buffer = [0u8; 4096];
        let count = common::read_stdin(&mut buffer);
        if count < 0 {
            common::write(b"cat: cannot read stdin\n");
            common::exit(1);
        }
        common::write(&buffer[..count as usize]);
        common::exit(0);
    }
    // Each file is read once, so output is limited to the 4 KiB buffer.
    let mut buffer = [0u8; 4096];
    for index in 1..argc {
        let Some(path) = (unsafe { common::argument(argv, index) }) else {
            continue;
        };
        let mut resolved = [0u8; 256];
        let Some(path_length) = common::resolve_path(envp, envc, path, &mut resolved) else {
            common::write(b"cat: path too long\n");
            continue;
        };
        let result = common::read_file(&resolved[..path_length], &mut buffer);
        if result < 0 {
            common::write(b"cat: cannot read file\n");
            continue;
        }
        common::write(&buffer[..result as usize]);
    }
    common::exit(0)
}
