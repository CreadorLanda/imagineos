#![no_std]
#![no_main]

use core::panic::PanicInfo;

#[unsafe(no_mangle)]
extern "C" fn _start(
    _argc: usize,
    _argv: *const *const u8,
    _envc: usize,
    _envp: *const *const u8,
) -> ! {
    loop {
        if imagineos::console::write_all(b"\nImagineOS login\nUsername: ").is_err() {
            imagineos::process::exit(1);
        }
        let mut username = [0u8; imagineos::abi::ACCOUNT_NAME_SIZE];
        let username_length = match read_line(&mut username, false) {
            Ok(length) => length,
            Err(_) => imagineos::process::exit(1),
        };
        if username_length == 0 {
            continue;
        }
        if imagineos::console::write_all(b"Password: ").is_err() {
            imagineos::process::exit(1);
        }
        let mut password = [0u8; imagineos::abi::ACCOUNT_PASSWORD_SIZE];
        let password_length = match read_line(&mut password, true) {
            Ok(length) => length,
            Err(_) => imagineos::process::exit(1),
        };
        let Ok(username_text) = core::str::from_utf8(&username[..username_length]) else {
            continue;
        };
        if imagineos::users::authenticate(username_text, &password[..password_length], u32::MAX)
            .is_err()
        {
            password.fill(0);
            let _ = imagineos::console::write_all(b"Login incorrect\n");
            continue;
        }
        password.fill(0);
        let identity = match imagineos::users::identity() {
            Ok(identity) => identity,
            Err(_) => imagineos::process::exit(1),
        };
        let hostname =
            core::str::from_utf8(&identity.hostname[..identity.hostname_length as usize])
                .unwrap_or("imagineos");
        let username =
            core::str::from_utf8(&identity.username[..identity.username_length as usize])
                .unwrap_or("root");
        let mut home_storage = [0u8; 48];
        home_storage[..6].copy_from_slice(b"/home/");
        let home_length = if identity.uid == 0 {
            5
        } else {
            6 + username.len()
        };
        if identity.uid == 0 {
            home_storage[..5].copy_from_slice(b"/home");
        } else {
            home_storage[6..home_length].copy_from_slice(username.as_bytes());
        }
        let home = core::str::from_utf8(&home_storage[..home_length]).unwrap_or("/home");
        let mut user_env = [0u8; 40];
        user_env[..5].copy_from_slice(b"USER=");
        user_env[5..5 + username.len()].copy_from_slice(username.as_bytes());
        let user_env = core::str::from_utf8(&user_env[..5 + username.len()]).unwrap_or("USER=root");
        let mut host_env = [0u8; 72];
        host_env[..9].copy_from_slice(b"HOSTNAME=");
        host_env[9..9 + hostname.len()].copy_from_slice(hostname.as_bytes());
        let host_env =
            core::str::from_utf8(&host_env[..9 + hostname.len()]).unwrap_or("HOSTNAME=imagineos");
        let mut home_env_storage = [0u8; 56];
        home_env_storage[..5].copy_from_slice(b"HOME=");
        home_env_storage[5..5 + home.len()].copy_from_slice(home.as_bytes());
        let home_env =
            core::str::from_utf8(&home_env_storage[..5 + home.len()]).unwrap_or("HOME=/home");
        let shell_pid = match imagineos::process::exec(
            "/bin/shell",
            &["/bin/shell"],
            &[user_env, host_env, home_env],
        ) {
            Ok(pid) => pid,
            Err(_) => {
                let _ = imagineos::console::write_all(b"getty: nao foi possivel iniciar o shell\n");
                continue;
            }
        };
        let mut processes = [imagineos::abi::ProcessInfo::default(); imagineos::abi::MAX_PROCESSES];
        loop {
            match imagineos::process::list(&mut processes) {
                Ok(count)
                    if processes
                        .iter()
                        .take(count)
                        .any(|process| process.pid == shell_pid as u64) =>
                {
                    if imagineos::process::yield_now().is_err() {
                        imagineos::process::exit(1);
                    }
                }
                Ok(_) => break,
                Err(_) => imagineos::process::exit(1),
            }
        }
    }
}

fn read_line(output: &mut [u8], mask: bool) -> imagineos::Result<usize> {
    let mut length = 0;
    loop {
        let character = imagineos::console::read_char()?;
        match character {
            '\r' | '\n' => {
                imagineos::console::write_all(b"\n")?;
                return Ok(length);
            }
            '\u{8}' | '\u{7f}' if length > 0 => {
                length -= 1;
                imagineos::console::write_all(b"\x08 \x08")?;
            }
            value if value.is_ascii() && !value.is_ascii_control() && length < output.len() => {
                output[length] = value as u8;
                length += 1;
                if mask {
                    imagineos::console::write_all(b"*")?;
                } else {
                    let byte = [value as u8];
                    imagineos::console::write_all(&byte)?;
                }
            }
            _ => {}
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    imagineos::process::exit(127)
}
