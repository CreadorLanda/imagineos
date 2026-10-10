use core::panic::PanicInfo;

#[allow(unused_imports)]
pub use imagineos::legacy::{
    argument, clear, environment_value, exit, list_directory, mkdir, read_char, read_fd, read_file,
    read_stdin, remove, resolve_path, touch, write, write_file,
};

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    imagineos::process::exit(127)
}
