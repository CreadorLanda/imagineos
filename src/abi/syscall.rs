use core::arch::asm;
use core::cell::UnsafeCell;

use crate::keyboard::Keyboard;
use crate::process::UserArg;
use crate::{framebuffer, process};

pub const SYS_WRITE: u64 = 1;
pub const SYS_READ: u64 = 2;
pub const SYS_YIELD: u64 = 3;
pub const SYS_EXIT: u64 = 4;
pub const SYS_GETPID: u64 = 5;
pub const SYS_CLEAR: u64 = 6;
pub const SYS_EXEC: u64 = 7;
pub const SYS_ISDIR: u64 = 8;
pub const SYS_ISFILE: u64 = 9;
pub const SYS_READ_FILE: u64 = 10;
pub const SYS_READDIR: u64 = 11;
pub const SYS_MKDIR: u64 = 12;
pub const SYS_TOUCH: u64 = 13;
pub const SYS_REMOVE: u64 = 14;

struct SharedKeyboard(UnsafeCell<Keyboard>);
unsafe impl Sync for SharedKeyboard {}
static KEYBOARD: SharedKeyboard = SharedKeyboard(UnsafeCell::new(Keyboard::new()));

#[repr(C)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

#[no_mangle]
extern "C" fn dreamcore_syscall_dispatch(frame: *mut TrapFrame) -> *mut TrapFrame {
    let Some(frame_ref) = (unsafe { frame.as_mut() }) else {
        return frame;
    };
    match frame_ref.rax {
        SYS_YIELD => {
            frame_ref.rax = 0;
            process::yield_current(frame)
        }
        SYS_EXIT => process::exit_current(frame),
        SYS_GETPID => {
            frame_ref.rax = process::current_pid() as u64;
            frame
        }
        SYS_WRITE => {
            let length = (frame_ref.rsi as usize).min(512);
            let mut buffer = [0u8; 512];
            if process::copy_from_current_user(frame_ref.rdi, &mut buffer[..length]) {
                crate::console_write_bytes(&buffer[..length]);
                frame_ref.rax = length as u64;
            } else {
                frame_ref.rax = (-14i64) as u64;
            }
            frame
        }
        SYS_READ => {
            frame_ref.rax = read_character() as u64;
            frame
        }
        SYS_CLEAR => {
            framebuffer::clear();
            frame_ref.rax = 0;
            frame
        }
        SYS_EXEC => {
            // Do not silently truncate: an overlong path would execute the
            // wrong file. Report -ENAMETOOLONG like copy_user_path does.
            if frame_ref.rsi as usize > 128 {
                frame_ref.rax = (-36i64) as u64;
                return frame;
            }
            let length = frame_ref.rsi as usize;
            let mut path = [0u8; 128];
            if !process::copy_from_current_user(frame_ref.rdi, &mut path[..length]) {
                frame_ref.rax = (-14i64) as u64;
                return frame;
            }
            let Ok(path) = core::str::from_utf8(&path[..length]) else {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            };
            let argument_count = frame_ref.r10 as usize;
            if argument_count > process::MAX_EXEC_ARGS {
                frame_ref.rax = (-7i64) as u64;
                return frame;
            }
            let mut user_arguments = [UserArg::EMPTY; process::MAX_EXEC_ARGS];
            let metadata_length = argument_count * core::mem::size_of::<UserArg>();
            let metadata = unsafe {
                core::slice::from_raw_parts_mut(
                    user_arguments.as_mut_ptr().cast::<u8>(),
                    metadata_length,
                )
            };
            if !process::copy_from_current_user(frame_ref.rdx, metadata) {
                frame_ref.rax = (-14i64) as u64;
                return frame;
            }

            let mut argument_storage = [[0u8; 128]; process::MAX_EXEC_ARGS];
            let mut argument_lengths = [0usize; process::MAX_EXEC_ARGS];
            for index in 0..argument_count {
                let Ok(length) = usize::try_from(user_arguments[index].length) else {
                    frame_ref.rax = (-7i64) as u64;
                    return frame;
                };
                if length > argument_storage[index].len()
                    || !process::copy_from_current_user(
                        user_arguments[index].address,
                        &mut argument_storage[index][..length],
                    )
                {
                    frame_ref.rax = (-14i64) as u64;
                    return frame;
                }
                argument_lengths[index] = length;
            }
            let mut arguments: [&[u8]; process::MAX_EXEC_ARGS] = [&[]; process::MAX_EXEC_ARGS];
            for index in 0..argument_count {
                arguments[index] = &argument_storage[index][..argument_lengths[index]];
            }

            let environment_count = frame_ref.r9 as usize;
            if environment_count > process::MAX_EXEC_ENV {
                frame_ref.rax = (-7i64) as u64;
                return frame;
            }
            let mut user_environment = [UserArg::EMPTY; process::MAX_EXEC_ENV];
            let metadata_length = environment_count * core::mem::size_of::<UserArg>();
            let metadata = unsafe {
                core::slice::from_raw_parts_mut(
                    user_environment.as_mut_ptr().cast::<u8>(),
                    metadata_length,
                )
            };
            if !process::copy_from_current_user(frame_ref.r8, metadata) {
                frame_ref.rax = (-14i64) as u64;
                return frame;
            }
            let mut environment_storage = [[0u8; 128]; process::MAX_EXEC_ENV];
            let mut environment_lengths = [0usize; process::MAX_EXEC_ENV];
            for index in 0..environment_count {
                let Ok(length) = usize::try_from(user_environment[index].length) else {
                    frame_ref.rax = (-7i64) as u64;
                    return frame;
                };
                if length > environment_storage[index].len()
                    || !process::copy_from_current_user(
                        user_environment[index].address,
                        &mut environment_storage[index][..length],
                    )
                {
                    frame_ref.rax = (-14i64) as u64;
                    return frame;
                }
                environment_lengths[index] = length;
            }
            let mut environment: [&[u8]; process::MAX_EXEC_ENV] = [&[]; process::MAX_EXEC_ENV];
            for index in 0..environment_count {
                environment[index] = &environment_storage[index][..environment_lengths[index]];
            }
            process::spawn_current(
                path,
                &arguments[..argument_count],
                &environment[..environment_count],
                frame,
            )
        }
        SYS_ISDIR => {
            if frame_ref.rsi as usize > 128 {
                frame_ref.rax = (-36i64) as u64;
                return frame;
            }
            let length = frame_ref.rsi as usize;
            let mut path = [0u8; 128];
            if !process::copy_from_current_user(frame_ref.rdi, &mut path[..length]) {
                frame_ref.rax = (-14i64) as u64;
                return frame;
            }
            let Ok(path) = core::str::from_utf8(&path[..length]) else {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            };
            frame_ref.rax = crate::ramfs::is_directory(path) as u64;
            frame
        }
        SYS_ISFILE => {
            if frame_ref.rsi as usize > 128 {
                frame_ref.rax = (-36i64) as u64;
                return frame;
            }
            let length = frame_ref.rsi as usize;
            let mut path = [0u8; 128];
            if !process::copy_from_current_user(frame_ref.rdi, &mut path[..length]) {
                frame_ref.rax = (-14i64) as u64;
                return frame;
            }
            let Ok(path) = core::str::from_utf8(&path[..length]) else {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            };
            frame_ref.rax = crate::ramfs::is_file(path) as u64;
            frame
        }
        SYS_READ_FILE | SYS_READDIR => {
            if frame_ref.rsi as usize > 128 {
                frame_ref.rax = (-36i64) as u64;
                return frame;
            }
            let path_length = frame_ref.rsi as usize;
            let capacity = (frame_ref.r10 as usize).min(4096);
            let mut path = [0u8; 128];
            if !process::copy_from_current_user(frame_ref.rdi, &mut path[..path_length]) {
                frame_ref.rax = (-14i64) as u64;
                return frame;
            }
            let Ok(path) = core::str::from_utf8(&path[..path_length]) else {
                frame_ref.rax = (-22i64) as u64;
                return frame;
            };
            let mut buffer = [0u8; 4096];
            let length = if frame_ref.rax == SYS_READ_FILE {
                let Some(bytes) = crate::ramfs::read(path) else {
                    frame_ref.rax = (-2i64) as u64;
                    return frame;
                };
                if bytes.len() > capacity {
                    frame_ref.rax = (-75i64) as u64;
                    return frame;
                }
                buffer[..bytes.len()].copy_from_slice(bytes);
                bytes.len()
            } else {
                let Some(length) = crate::ramfs::list_directory(path, &mut buffer[..capacity])
                else {
                    frame_ref.rax = (-75i64) as u64;
                    return frame;
                };
                length
            };
            if !process::copy_to_current_user(frame_ref.rdx, &buffer[..length]) {
                frame_ref.rax = (-14i64) as u64;
            } else {
                frame_ref.rax = length as u64;
            }
            frame
        }
        SYS_MKDIR | SYS_TOUCH | SYS_REMOVE => {
            let mut path_buffer = [0u8; 256];
            let path = match copy_user_path(frame_ref.rdi, frame_ref.rsi, &mut path_buffer) {
                Ok(path) => path,
                Err(error) => {
                    frame_ref.rax = error as u64;
                    return frame;
                }
            };
            let result = match frame_ref.rax {
                SYS_MKDIR if frame_ref.rdx != 0 => {
                    crate::ramfs::create_directory_with_parents(path, true)
                }
                SYS_MKDIR => crate::ramfs::create_directory(path),
                SYS_TOUCH => crate::ramfs::create_file(path),
                SYS_REMOVE => crate::ramfs::remove(path, frame_ref.rdx != 0),
                _ => unreachable!(),
            };
            frame_ref.rax = result.map_or_else(fs_error_code, |_| 0) as u64;
            frame
        }
        _ => {
            // Unknown syscall number: -ENOSYS, matching the negative-errno
            // convention used by every other arm (-2, -14, -22, ...).
            frame_ref.rax = (-38i64) as u64;
            frame
        }
    }
}

fn copy_user_path<'a>(
    address: u64,
    length: u64,
    buffer: &'a mut [u8; 256],
) -> Result<&'a str, i64> {
    let length = usize::try_from(length).map_err(|_| -22i64)?;
    if length == 0 || length > buffer.len() {
        return Err(-22);
    }
    if !process::copy_from_current_user(address, &mut buffer[..length]) {
        return Err(-14);
    }
    core::str::from_utf8(&buffer[..length]).map_err(|_| -22)
}

fn fs_error_code(error: crate::ramfs::FsError) -> i64 {
    match error {
        crate::ramfs::FsError::InvalidPath => -22,
        crate::ramfs::FsError::NotFound => -2,
        crate::ramfs::FsError::NotDirectory => -20,
        crate::ramfs::FsError::IsDirectory => -21,
        crate::ramfs::FsError::AlreadyExists => -17,
        crate::ramfs::FsError::DirectoryNotEmpty => -39,
        crate::ramfs::FsError::NoSpace => -28,
    }
}

fn read_character() -> u32 {
    const BLINK_INTERVAL: u64 = 750_000_000;
    let mut last_blink = read_tsc();
    framebuffer::set_cursor_visible(true);
    let mut cursor_visible = true;
    loop {
        if let Some(character) = unsafe { (&mut *KEYBOARD.0.get()).poll_char() } {
            framebuffer::set_cursor_visible(false);
            return character as u32;
        }
        if let Some(character) = serial_read_char() {
            framebuffer::set_cursor_visible(false);
            return character as u32;
        }
        let now = read_tsc();
        if now.wrapping_sub(last_blink) >= BLINK_INTERVAL {
            cursor_visible = !cursor_visible;
            framebuffer::set_cursor_visible(cursor_visible);
            last_blink = now;
        }
        core::hint::spin_loop();
    }
}

fn read_tsc() -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!(
            "rdtsc",
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags)
        );
    }
    ((high as u64) << 32) | low as u64
}

fn serial_read_char() -> Option<char> {
    unsafe {
        if in_port(0x3fd) & 1 != 0 {
            Some(in_port(0x3f8) as char)
        } else {
            None
        }
    }
}

unsafe fn in_port(port: u16) -> u8 {
    let value: u8;
    asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    value
}
