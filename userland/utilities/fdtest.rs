#![no_std]
#![no_main]

fn run() -> imagineos::Result<()> {
    use imagineos::fs::{self, OpenOptions};

    const PATH: &str = "/tmp/fdtest.txt";
    const INITIAL: &[u8] = b"file descriptors";
    const APPENDED: &[u8] = b" work";

    let mut output = fs::open(
        PATH,
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true),
    )?;
    if output.write(INITIAL)? != INITIAL.len() {
        return Err(imagineos::Error::from_errno(5));
    }
    output.close()?;

    let mut input = fs::open(PATH, OpenOptions::new().read(true))?;
    let mut contents = [0u8; 64];
    let length = input.read(&mut contents)?;
    if &contents[..length] != INITIAL {
        return Err(imagineos::Error::from_errno(5));
    }
    let input_fd = input.descriptor();
    input.close()?;
    if fs::read(input_fd, &mut contents).is_ok() {
        return Err(imagineos::Error::from_errno(5));
    }

    let mut append = fs::open(
        PATH,
        OpenOptions::new().write(true).append(true),
    )?;
    if append.write(APPENDED)? != APPENDED.len() {
        return Err(imagineos::Error::from_errno(5));
    }
    append.close()?;

    let mut input = fs::open(PATH, OpenOptions::new().read(true))?;
    let length = input.read(&mut contents)?;
    input.close()?;
    if &contents[..length] != b"file descriptors work" {
        return Err(imagineos::Error::from_errno(5));
    }
    fs::remove(PATH, false)?;
    imagineos::console::write_all(b"FD test passed: stdio, open/read/write/append/close\n")?;
    Ok(())
}

#[unsafe(no_mangle)]
extern "C" fn _start(
    _argc: usize,
    _argv: *const *const u8,
    _envc: usize,
    _envp: *const *const u8,
) -> ! {
    if imagineos::syscall::abi_version() != Ok(imagineos::abi::ABI_VERSION) {
        imagineos::process::exit(2);
    }
    if let Err(error) = run() {
        let _ = imagineos::console::write_error(b"fdtest: descriptor operation failed\n");
        imagineos::process::exit(error.code() as i32);
    }
    imagineos::process::exit(0)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    imagineos::process::exit(127)
}
