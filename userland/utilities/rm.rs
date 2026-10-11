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
    let mut index = 1usize;
    let recursive = if argc > 1 {
        match unsafe { common::argument(argv, 1) } {
            Some(b"-r" | b"-R") => {
                index = 2;
                true
            }
            _ => false,
        }
    } else {
        false
    };
    if index >= argc {
        common::write(b"usage: rm [-r] PATH...\n");
        common::exit(2);
    }
    let mut failed = false;
    while index < argc {
        if let Some(argument) = unsafe { common::argument(argv, index) } {
            let mut path = [0u8; 256];
            match common::resolve_path(envp, envc, argument, &mut path) {
                Some(length) if common::remove(&path[..length], recursive) >= 0 => {}
                _ => {
                    common::write(b"rm: cannot remove path\n");
                    failed = true;
                }
            }
        }
        index += 1;
    }
    common::exit(if failed { 1 } else { 0 })
}