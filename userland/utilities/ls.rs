#![no_std]
#![no_main]

#[allow(dead_code)]
mod common;

#[derive(Clone, Copy)]
struct Options {
    all: bool,
    size: bool,
    file_type: bool,
    owner: bool,
    permissions: bool,
    human_size: bool,
}

impl Options {
    const fn names_only() -> Self {
        Self {
            all: false,
            size: false,
            file_type: false,
            owner: false,
            permissions: false,
            human_size: false,
        }
    }

    fn long(&mut self) {
        self.size = true;
        self.file_type = true;
        self.owner = true;
        self.permissions = true;
    }
}

#[unsafe(no_mangle)]
extern "C" fn _start(
    argc: usize,
    argv: *const *const u8,
    envc: usize,
    envp: *const *const u8,
) -> ! {
    let mut options = Options::names_only();
    let mut requested: Option<&[u8]> = None;
    for index in 1..argc {
        let Some(argument) = (unsafe { common::argument(argv, index) }) else {
            common::write(b"ls: invalid argument list\n");
            common::exit(2);
        };
        if argument == b"-a" || argument == b"--all" {
            options.all = true;
        } else if argument == b"-l" || argument == b"--long" {
            options.long();
        } else if argument == b"-s" || argument == b"--size" {
            options.size = true;
        } else if argument == b"-t" || argument == b"--type" {
            options.file_type = true;
        } else if argument == b"-o" || argument == b"--owner" {
            options.owner = true;
        } else if argument == b"-p" || argument == b"--permissions" {
            options.permissions = true;
        } else if argument == b"-h" || argument == b"--human-readable" {
            options.human_size = true;
            options.size = true;
        } else if argument.starts_with(b"-") {
            common::write(b"ls: unknown option\n");
            common::exit(2);
        } else if requested.replace(argument).is_some() {
            common::write(b"ls: only one directory path is supported\n");
            common::exit(2);
        }
    }

    let requested = requested.unwrap_or_else(|| unsafe {
        common::environment_value(envp, envc, b"PWD").unwrap_or(b"/")
    });
    let mut path = [0u8; 256];
    let Some(path_length) = common::resolve_path(envp, envc, requested, &mut path) else {
        common::write(b"ls: path too long\n");
        common::exit(2);
    };
    let mut entries = [0u8; 4096];
    let length = match imagineos::fs::list_directory_with_hidden(
        core::str::from_utf8(&path[..path_length]).unwrap_or("/"),
        &mut entries,
        options.all,
    ) {
        Ok(length) => length,
        Err(_) => {
            common::write(b"ls: cannot list directory\n");
            common::exit(2);
        }
    };

    for name in entries[..length].split(|byte| *byte == b'\n') {
        if name.is_empty() {
            continue;
        }
        let mut child = [0u8; 256];
        let Some(child_length) =
            imagineos::args::append_path_component(&path[..path_length], name, &mut child)
        else {
            common::write(b"ls: path too long\n");
            common::exit(2);
        };
        let child_path = core::str::from_utf8(&child[..child_length]).unwrap_or("/");
        let metadata = match imagineos::fs::metadata(child_path) {
            Ok(metadata) => metadata,
            Err(_) => {
                common::write(b"ls: cannot read entry metadata\n");
                common::exit(2);
            }
        };
        write_entry(name, metadata, options);
    }
    common::exit(0)
}

fn write_entry(name: &[u8], metadata: imagineos::fs::Metadata, options: Options) {
    if options.permissions {
        write_permissions(metadata.mode, metadata.kind != 0);
        common::write(b"  ");
    }
    if options.owner {
        write_number(metadata.uid as u64);
        common::write(b":");
        write_number(metadata.gid as u64);
        common::write(b"  ");
    }
    if options.file_type {
        common::write(if metadata.kind != 0 {
            b"directory  "
        } else {
            b"file       "
        });
    }
    if options.size {
        if options.human_size {
            write_human_size(metadata.size);
        } else {
            write_number(metadata.size);
        }
        common::write(b"  ");
    }
    common::write(name);
    common::write(b"\n");
}

fn write_permissions(mode: u32, directory: bool) {
    common::write(if directory { b"d" } else { b"-" });
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 7;
        for (mask, character) in [(4, b'r'), (2, b'w'), (1, b'x')] {
            common::write(if bits & mask != 0 {
                core::slice::from_ref(&character)
            } else {
                b"-"
            });
        }
    }
}

fn write_human_size(size: u64) {
    const UNITS: [&[u8]; 4] = [b"B", b"KiB", b"MiB", b"GiB"];
    let mut value = size;
    let mut unit = 0;
    while value >= 1024 && unit < UNITS.len() - 1 {
        value /= 1024;
        unit += 1;
    }
    write_number(value);
    common::write(b" ");
    common::write(UNITS[unit]);
}

fn write_number(mut value: u64) {
    let mut digits = [0u8; 20];
    let mut cursor = digits.len();
    if value == 0 {
        common::write(b"0");
        return;
    }
    while value != 0 {
        cursor -= 1;
        digits[cursor] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    common::write(&digits[cursor..]);
}
