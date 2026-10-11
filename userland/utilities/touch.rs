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
    if argc < 2 {
        common::write(b"usage: touch FILE...\n");
        common::exit(2);
    }
    let mut failed = false;
    for index in 1..argc {
        if let Some(argument) = unsafe { common::argument(argv, index) } {
            let mut path = [0u8; 256];
            match common::resolve_path(envp, envc, argument, &mut path) {
                Some(length) if common::touch(&path[..length]) >= 0 => {}
                _ => {
                    common::write(b"touch: cannot create file\n");
                    failed = true;
                }
            }
        }
    }
    common::exit(if failed { 1 } else { 0 })
}