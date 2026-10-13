#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

const SYS_WRITE: u64 = 1;
const SYS_READ: u64 = 2;
const SYS_YIELD: u64 = 3;
const SYS_EXIT: u64 = 4;
const SYS_GETPID: u64 = 5;
const SYS_CLEAR: u64 = 6;
const SYS_EXEC: u64 = 7;
const SYS_ISDIR: u64 = 8;
const SYS_ISFILE: u64 = 9;
const MAX_ARGS: usize = 12;
const TOKEN_SIZE: usize = 128;
const MAX_VARIABLES: usize = 12;
const VARIABLE_NAME_SIZE: usize = 24;
const VARIABLE_VALUE_SIZE: usize = 96;

#[derive(Clone, Copy)]
struct Token {
    bytes: [u8; TOKEN_SIZE],
    length: usize,
}

impl Token {
    const EMPTY: Self = Self {
        bytes: [0; TOKEN_SIZE],
        length: 0,
    };

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }

    fn push(&mut self, byte: u8) -> Result<(), ()> {
        if self.length == self.bytes.len() {
            return Err(());
        }
        self.bytes[self.length] = byte;
        self.length += 1;
        Ok(())
    }

    fn extend(&mut self, bytes: &[u8]) -> Result<(), ()> {
        if self.length + bytes.len() > self.bytes.len() {
            return Err(());
        }
        self.bytes[self.length..self.length + bytes.len()].copy_from_slice(bytes);
        self.length += bytes.len();
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Variable {
    name: [u8; VARIABLE_NAME_SIZE],
    name_length: usize,
    value: [u8; VARIABLE_VALUE_SIZE],
    value_length: usize,
    active: bool,
}

impl Variable {
    const EMPTY: Self = Self {
        name: [0; VARIABLE_NAME_SIZE],
        name_length: 0,
        value: [0; VARIABLE_VALUE_SIZE],
        value_length: 0,
        active: false,
    };
}

struct ShellState {
    cwd: [u8; TOKEN_SIZE],
    cwd_length: usize,
    variables: [Variable; MAX_VARIABLES],
}

impl ShellState {
    fn new() -> Self {
        let mut state = Self {
            cwd: [0; TOKEN_SIZE],
            cwd_length: 1,
            variables: [Variable::EMPTY; MAX_VARIABLES],
        };
        state.cwd[0] = b'/';
        state.set_variable(b"HOME", b"/home");
        state.set_variable(b"PATH", b"/bin");
        state.set_variable(b"PWD", b"/");
        state
    }

    fn get_variable(&self, name: &[u8]) -> Option<&[u8]> {
        self.variables
            .iter()
            .find(|variable| variable.active && &variable.name[..variable.name_length] == name)
            .map(|variable| &variable.value[..variable.value_length])
    }

    fn set_variable(&mut self, name: &[u8], value: &[u8]) -> bool {
        if !valid_identifier(name)
            || name.len() > VARIABLE_NAME_SIZE
            || value.len() > VARIABLE_VALUE_SIZE
        {
            return false;
        }
        let slot = self
            .variables
            .iter()
            .position(|variable| variable.active && &variable.name[..variable.name_length] == name)
            .or_else(|| self.variables.iter().position(|variable| !variable.active));
        let Some(slot) = slot else { return false };
        let variable = &mut self.variables[slot];
        variable.name = [0; VARIABLE_NAME_SIZE];
        variable.value = [0; VARIABLE_VALUE_SIZE];
        variable.name[..name.len()].copy_from_slice(name);
        variable.value[..value.len()].copy_from_slice(value);
        variable.name_length = name.len();
        variable.value_length = value.len();
        variable.active = true;
        true
    }

    fn unset_variable(&mut self, name: &[u8]) {
        if let Some(variable) = self
            .variables
            .iter_mut()
            .find(|variable| variable.active && &variable.name[..variable.name_length] == name)
        {
            *variable = Variable::EMPTY;
        }
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
#[allow(dead_code)]
struct UserArg {
    address: u64,
    length: u64,
}

#[no_mangle]
extern "C" fn _start(
    _argc: usize,
    _argv: *const *const u8,
    _envc: usize,
    _envp: *const *const u8,
) -> ! {
    let mut state = ShellState::new();
    let mut line = [0u8; 128];
    let mut length = 0usize;
    prompt(&state);

    loop {
        let codepoint = syscall0(SYS_READ) as u32;
        let Some(character) = char::from_u32(codepoint) else {
            continue;
        };
        match character {
            '\r' | '\n' => {
                write("\n");
                run_command(&mut state, &line[..length]);
                line.fill(0);
                length = 0;
                prompt(&state);
            }
            '\u{8}' | '\u{7f}' => {
                if length > 0 {
                    length -= 1;
                    while length > 0 && line[length] & 0xc0 == 0x80 {
                        length -= 1;
                    }
                    write("\u{8} \u{8}");
                }
            }
            value if !value.is_control() => {
                let mut encoded = [0u8; 4];
                let bytes = value.encode_utf8(&mut encoded).as_bytes();
                if length + bytes.len() <= line.len() {
                    line[length..length + bytes.len()].copy_from_slice(bytes);
                    length += bytes.len();
                    unsafe {
                        syscall3(SYS_WRITE, bytes.as_ptr() as u64, bytes.len() as u64, 0);
                    }
                }
            }
            _ => {}
        }
        syscall0(SYS_YIELD);
    }
}

fn prompt(state: &ShellState) {
    write("Astrid:");
    write_bytes(&state.cwd[..state.cwd_length]);
    write("$ ");
}

fn run_command(state: &mut ShellState, line: &[u8]) {
    let mut tokens = [Token::EMPTY; MAX_ARGS];
    let count = match parse_line(state, line, &mut tokens) {
        Ok(count) => count,
        Err(()) => {
            write("syntax error or token too long\n");
            return;
        }
    };
    if count == 0 {
        return;
    }
    let command = tokens[0].as_bytes();
    let arguments = &tokens[1..count];
    match command {
        b"help" => write("cd pwd echo export unset set read clear pid type exit; other commands are searched in PATH\n"),
        b"clear" => { syscall0(SYS_CLEAR); }
        b"pid" => print_number(syscall0(SYS_GETPID)),
        b"pwd" => {
            write_bytes(&state.cwd[..state.cwd_length]);
            write("\n");
        }
        b"cd" => builtin_cd(state, arguments),
        b"echo" => builtin_echo(arguments),
        b"export" => builtin_export(state, arguments),
        b"unset" => builtin_unset(state, arguments),
        b"set" => print_variables(state),
        b"read" => builtin_read(state, arguments),
        b"type" => builtin_type(state, arguments),
        b"exit" | b"logout" => { syscall0(SYS_EXIT); }
        _ => run_external(state, &tokens[..count]),
    }
}

fn parse_line(
    state: &ShellState,
    line: &[u8],
    tokens: &mut [Token; MAX_ARGS],
) -> Result<usize, ()> {
    let mut position = 0usize;
    let mut count = 0usize;
    while position < line.len() {
        while position < line.len() && line[position].is_ascii_whitespace() {
            position += 1;
        }
        if position == line.len() {
            break;
        }
        if count == tokens.len() {
            return Err(());
        }

        let token = &mut tokens[count];
        let mut quote = 0u8;
        let mut started = false;
        while position < line.len() {
            let byte = line[position];
            if quote == 0 && byte.is_ascii_whitespace() {
                break;
            }
            if quote == 0 && (byte == b'\'' || byte == b'"') {
                quote = byte;
                started = true;
                position += 1;
                continue;
            }
            if quote == byte {
                quote = 0;
                position += 1;
                continue;
            }
            if byte == b'\\' && quote != b'\'' {
                position += 1;
                if position == line.len() {
                    return Err(());
                }
                token.push(line[position])?;
                started = true;
                position += 1;
                continue;
            }
            if byte == b'$' && quote != b'\'' {
                let (name, next) = variable_reference(line, position + 1);
                if name.is_empty() {
                    token.push(b'$')?;
                    position += 1;
                } else {
                    if let Some(value) = state.get_variable(name) {
                        token.extend(value)?;
                    }
                    started = true;
                    position = next;
                }
                continue;
            }
            token.push(byte)?;
            started = true;
            position += 1;
        }
        if quote != 0 {
            return Err(());
        }
        if started {
            count += 1;
        }
        while position < line.len() && line[position].is_ascii_whitespace() {
            position += 1;
        }
    }
    Ok(count)
}

fn variable_reference(line: &[u8], start: usize) -> (&[u8], usize) {
    if line.get(start) == Some(&b'{') {
        let name_start = start + 1;
        if let Some(relative_end) = line[name_start..].iter().position(|byte| *byte == b'}') {
            let end = name_start + relative_end;
            return (&line[name_start..end], end + 1);
        }
        return (&[], start);
    }
    let mut end = start;
    while end < line.len() && (line[end].is_ascii_alphanumeric() || line[end] == b'_') {
        end += 1;
    }
    (&line[start..end], end)
}

fn valid_identifier(name: &[u8]) -> bool {
    !name.is_empty()
        && (name[0].is_ascii_alphabetic() || name[0] == b'_')
        && name
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

fn builtin_cd(state: &mut ShellState, arguments: &[Token]) {
    if arguments.len() > 1 {
        write("cd: expected at most one path\n");
        return;
    }
    let requested = if arguments.is_empty() {
        state.get_variable(b"HOME").unwrap_or(b"/")
    } else {
        arguments[0].as_bytes()
    };
    let mut path = [0u8; TOKEN_SIZE];
    let Some(length) = normalize_path(state, requested, &mut path) else {
        write("cd: path too long\n");
        return;
    };
    if unsafe { syscall2(SYS_ISDIR, path.as_ptr() as u64, length as u64) } != 1 {
        write("cd: directory not found\n");
        return;
    }
    state.cwd = path;
    state.cwd_length = length;
    let pwd = state.cwd;
    if !state.set_variable(b"PWD", &pwd[..length]) {
        write("cd: could not update PWD\n");
    }
}

fn normalize_path(state: &ShellState, path: &[u8], output: &mut [u8; TOKEN_SIZE]) -> Option<usize> {
    let mut input = [0u8; TOKEN_SIZE * 2];
    let path = if path == b"~" {
        state.get_variable(b"HOME").unwrap_or(b"/")
    } else if path.starts_with(b"~/") {
        let home = state.get_variable(b"HOME").unwrap_or(b"/");
        let mut length = home.len();
        if length > input.len() {
            return None;
        }
        input[..length].copy_from_slice(home);
        if length > 1 && input[length - 1] == b'/' {
            length -= 1;
        }
        let suffix = &path[2..];
        if length + 1 + suffix.len() > input.len() {
            return None;
        }
        input[length] = b'/';
        length += 1;
        input[length..length + suffix.len()].copy_from_slice(suffix);
        &input[..length + suffix.len()]
    } else {
        path
    };

    let mut joined = [0u8; TOKEN_SIZE * 2];
    let joined_length = if path.starts_with(b"/") {
        if path.len() > joined.len() {
            return None;
        }
        joined[..path.len()].copy_from_slice(path);
        path.len()
    } else {
        let cwd = &state.cwd[..state.cwd_length];
        let mut length = cwd.len();
        joined[..length].copy_from_slice(cwd);
        if length == 0 || joined[length - 1] != b'/' {
            joined[length] = b'/';
            length += 1;
        }
        if length + path.len() > joined.len() {
            return None;
        }
        joined[length..length + path.len()].copy_from_slice(path);
        length + path.len()
    };

    output[0] = b'/';
    let mut output_length = 1usize;
    let mut component_starts = [0usize; 64];
    let mut component_count = 0usize;
    let mut position = 0usize;
    while position < joined_length {
        while position < joined_length && joined[position] == b'/' {
            position += 1;
        }
        let start = position;
        while position < joined_length && joined[position] != b'/' {
            position += 1;
        }
        let component = &joined[start..position];
        if component.is_empty() || component == b"." {
            continue;
        }
        if component == b".." {
            if component_count > 0 {
                component_count -= 1;
                output_length = component_starts[component_count];
            }
            continue;
        }
        if component_count == component_starts.len() {
            return None;
        }
        if output_length > 1 {
            if output_length == output.len() {
                return None;
            }
            output[output_length] = b'/';
            output_length += 1;
        }
        component_starts[component_count] = output_length.saturating_sub(1).max(1);
        if output_length + component.len() > output.len() {
            return None;
        }
        output[output_length..output_length + component.len()].copy_from_slice(component);
        output_length += component.len();
        component_count += 1;
    }
    Some(output_length)
}

fn builtin_echo(arguments: &[Token]) {
    let mut start = 0usize;
    let newline = !(arguments
        .first()
        .is_some_and(|token| token.as_bytes() == b"-n"));
    if !newline && !arguments.is_empty() {
        start = 1;
    }
    for (index, token) in arguments.iter().skip(start).enumerate() {
        if index > 0 {
            write(" ");
        }
        write_bytes(token.as_bytes());
    }
    if newline {
        write("\n");
    }
}

fn builtin_export(state: &mut ShellState, arguments: &[Token]) {
    if arguments.is_empty() {
        print_variables(state);
        return;
    }
    for token in arguments {
        let bytes = token.as_bytes();
        if let Some(equal) = bytes.iter().position(|byte| *byte == b'=') {
            if !state.set_variable(&bytes[..equal], &bytes[equal + 1..]) {
                write("export: invalid name or value too long\n");
            }
        } else if let Some(value) = state.get_variable(bytes) {
            write_bytes(bytes);
            write("=");
            write_bytes(value);
            write("\n");
        } else {
            write("export: expected NAME=VALUE\n");
        }
    }
}

fn builtin_unset(state: &mut ShellState, arguments: &[Token]) {
    for token in arguments {
        state.unset_variable(token.as_bytes());
    }
}

fn print_variables(state: &ShellState) {
    for variable in state.variables.iter().filter(|variable| variable.active) {
        write_bytes(&variable.name[..variable.name_length]);
        write("=");
        write_bytes(&variable.value[..variable.value_length]);
        write("\n");
    }
}

fn builtin_read(state: &mut ShellState, arguments: &[Token]) {
    if arguments.len() != 1 || !valid_identifier(arguments[0].as_bytes()) {
        write("read: expected one variable name\n");
        return;
    }
    let mut value = [0u8; VARIABLE_VALUE_SIZE];
    let mut length = 0usize;
    loop {
        let Some(character) = char::from_u32(syscall0(SYS_READ) as u32) else {
            continue;
        };
        match character {
            '\r' | '\n' => break,
            '\u{8}' | '\u{7f}' => {
                if length > 0 {
                    length -= 1;
                    while length > 0 && value[length] & 0xc0 == 0x80 {
                        length -= 1;
                    }
                    write("\u{8} \u{8}");
                }
            }
            character if !character.is_control() => {
                let mut encoded = [0u8; 4];
                let bytes = character.encode_utf8(&mut encoded).as_bytes();
                if length + bytes.len() <= value.len() {
                    value[length..length + bytes.len()].copy_from_slice(bytes);
                    length += bytes.len();
                    write_bytes(bytes);
                }
            }
            _ => {}
        }
    }
    write("\n");
    if !state.set_variable(arguments[0].as_bytes(), &value[..length]) {
        write("read: variable table full\n");
    }
}

fn builtin_type(state: &ShellState, arguments: &[Token]) {
    for token in arguments {
        let name = token.as_bytes();
        if is_builtin(name) {
            write_bytes(name);
            write(" is a shell builtin\n");
        } else {
            let mut path = [0u8; TOKEN_SIZE];
            if let Some(length) = find_external(state, name, &mut path) {
                write_bytes(name);
                write(" is ");
                write_bytes(&path[..length]);
                write("\n");
            } else {
                write_bytes(name);
                write(" not found\n");
            }
        }
    }
}

fn is_builtin(name: &[u8]) -> bool {
    matches!(
        name,
        b"cd"
            | b"pwd"
            | b"echo"
            | b"export"
            | b"unset"
            | b"set"
            | b"read"
            | b"clear"
            | b"pid"
            | b"type"
            | b"help"
            | b"exit"
            | b"logout"
    )
}

fn run_external(state: &ShellState, tokens: &[Token]) {
    let command = tokens[0].as_bytes();
    let mut path = [0u8; TOKEN_SIZE];
    let Some(path_length) = find_external(state, command, &mut path) else {
        write("command not found\n");
        return;
    };
    let result = if command == b"ls" && tokens.len() == 1 {
        let mut arguments = [Token::EMPTY; 2];
        arguments[0] = tokens[0];
        if arguments[1].extend(&state.cwd[..state.cwd_length]).is_err() {
            write("ls: current path too long\n");
            return;
        }
        execute_path(state, &path[..path_length], &arguments)
    } else {
        execute_path(state, &path[..path_length], tokens)
    };
    if result < 0 {
        if result == -2 {
            write("command not found\n");
        } else {
            write("cannot execute command\n");
        }
    }
}

fn find_external(
    state: &ShellState,
    command: &[u8],
    output: &mut [u8; TOKEN_SIZE],
) -> Option<usize> {
    if command.contains(&b'/') {
        let length = normalize_path(state, command, output)?;
        return (unsafe { syscall2(SYS_ISFILE, output.as_ptr() as u64, length as u64) } == 1)
            .then_some(length);
    }
    let path = state.get_variable(b"PATH")?;
    let mut start = 0usize;
    while start <= path.len() {
        let end = path[start..]
            .iter()
            .position(|byte| *byte == b':')
            .map_or(path.len(), |offset| start + offset);
        let directory = if end == start {
            &state.cwd[..state.cwd_length]
        } else {
            &path[start..end]
        };
        let mut candidate = [0u8; TOKEN_SIZE];
        if let Some(length) = join_path(directory, command, &mut candidate) {
            if unsafe { syscall2(SYS_ISFILE, candidate.as_ptr() as u64, length as u64) } == 1 {
                output[..length].copy_from_slice(&candidate[..length]);
                return Some(length);
            }
        }
        if end == path.len() {
            break;
        }
        start = end + 1;
    }
    None
}

fn join_path(directory: &[u8], command: &[u8], output: &mut [u8; TOKEN_SIZE]) -> Option<usize> {
    if directory.len() > output.len() {
        return None;
    }
    let mut length = 0usize;
    output[..directory.len()].copy_from_slice(directory);
    length += directory.len();
    if length == 0 || output[length - 1] != b'/' {
        if length >= output.len() {
            return None;
        }
        output[length] = b'/';
        length += 1;
    }
    if length + command.len() > output.len() {
        return None;
    }
    output[length..length + command.len()].copy_from_slice(command);
    Some(length + command.len())
}

fn execute_path(state: &ShellState, path: &[u8], tokens: &[Token]) -> i64 {
    let mut argv_metadata = [UserArg {
        address: 0,
        length: 0,
    }; MAX_ARGS];
    for (index, token) in tokens.iter().enumerate() {
        argv_metadata[index] = UserArg {
            address: token.bytes.as_ptr() as u64,
            length: token.length as u64,
        };
    }
    let mut environment_storage =
        [[0u8; VARIABLE_NAME_SIZE + VARIABLE_VALUE_SIZE + 1]; MAX_VARIABLES];
    let mut environment_metadata = [UserArg {
        address: 0,
        length: 0,
    }; MAX_VARIABLES];
    let mut environment_count = 0usize;
    for variable in state.variables.iter().filter(|variable| variable.active) {
        let storage = &mut environment_storage[environment_count];
        storage[..variable.name_length].copy_from_slice(&variable.name[..variable.name_length]);
        storage[variable.name_length] = b'=';
        storage[variable.name_length + 1..variable.name_length + 1 + variable.value_length]
            .copy_from_slice(&variable.value[..variable.value_length]);
        environment_metadata[environment_count] = UserArg {
            address: storage.as_ptr() as u64,
            length: (variable.name_length + 1 + variable.value_length) as u64,
        };
        environment_count += 1;
    }
    unsafe {
        syscall6(
            SYS_EXEC,
            path.as_ptr() as u64,
            path.len() as u64,
            argv_metadata.as_ptr() as u64,
            tokens.len() as u64,
            environment_metadata.as_ptr() as u64,
            environment_count as u64,
        ) as i64
    }
}

fn write(text: &str) {
    write_bytes(text.as_bytes());
}

fn write_bytes(bytes: &[u8]) {
    unsafe {
        syscall3(SYS_WRITE, bytes.as_ptr() as u64, bytes.len() as u64, 0);
    }
}

fn syscall0(number: u64) -> u64 {
    unsafe { syscall3(number, 0, 0, 0) }
}

unsafe fn syscall2(number: u64, first: u64, second: u64) -> u64 {
    let result: u64;
    asm!(
        "int 0x80",
        inout("rax") number => result,
        in("rdi") first,
        in("rsi") second,
    );
    result
}

unsafe fn syscall3(number: u64, first: u64, second: u64, third: u64) -> u64 {
    let result: u64;
    asm!(
        "int 0x80",
        inout("rax") number => result,
        in("rdi") first,
        in("rsi") second,
        in("rdx") third,
    );
    result
}

unsafe fn syscall6(
    number: u64,
    first: u64,
    second: u64,
    third: u64,
    fourth: u64,
    fifth: u64,
    sixth: u64,
) -> u64 {
    let result: u64;
    asm!(
        "int 0x80",
        inout("rax") number => result,
        in("rdi") first,
        in("rsi") second,
        in("rdx") third,
        in("r10") fourth,
        in("r8") fifth,
        in("r9") sixth,
    );
    result
}

fn print_number(mut value: u64) {
    let mut digits = [0u8; 20];
    let mut position = digits.len();
    if value == 0 {
        write("0\n");
        return;
    }
    while value != 0 {
        position -= 1;
        digits[position] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    write_bytes(&digits[position..]);
    write("\n");
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
