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
    // `grep PATTERN [FILE...]`; sem arquivo, filtra o stdin — o que torna
    // `algo | grep padrao` um pipeline de verdade.
    if argc < 2 {
        common::write(b"usage: grep PATTERN [FILE...]\n");
        common::exit(2);
    }
    let Some(pattern) = (unsafe { common::argument(argv, 1) }) else {
        common::exit(2);
    };
    let mut contents = [0u8; 4096];
    let length = if argc < 3 {
        common::read_stdin(&mut contents)
    } else {
        let Some(path) = (unsafe { common::argument(argv, 2) }) else {
            common::exit(2);
        };
        let mut resolved = [0u8; 256];
        let Some(path_length) = common::resolve_path(envp, envc, path, &mut resolved) else {
            common::write(b"grep: path too long\n");
            common::exit(2);
        };
        common::read_file(&resolved[..path_length], &mut contents)
    };
    if length < 0 {
        common::write(b"grep: cannot read input\n");
        common::exit(2);
    }
    if pattern.is_empty() {
        common::write(&contents[..length as usize]);
        common::exit(0);
    }
    for line in contents[..length as usize].split_inclusive(|byte| *byte == b'\n') {
        if line.windows(pattern.len()).any(|window| window == pattern) {
            common::write(line);
        }
    }
    common::exit(0)
}
