#![no_std]
#![no_main]

#[allow(dead_code)]
mod common;

const CAPACITY: usize = 4096;
const VISIBLE_LINES: usize = 18;
const KEY_UP: char = '\u{f0000}';
const KEY_DOWN: char = '\u{f0001}';
const KEY_LEFT: char = '\u{f0002}';
const KEY_RIGHT: char = '\u{f0003}';
const KEY_HOME: char = '\u{f0004}';
const KEY_END: char = '\u{f0005}';
const KEY_DELETE: char = '\u{f0006}';

#[unsafe(no_mangle)]
extern "C" fn _start(
    argc: usize,
    argv: *const *const u8,
    envc: usize,
    envp: *const *const u8,
) -> ! {
    if argc != 2 {
        common::write(b"usage: vi FILE\n");
        common::exit(2);
    }
    let Some(argument) = (unsafe { common::argument(argv, 1) }) else {
        common::write(b"vi: invalid path\n");
        common::exit(2);
    };
    let mut path = [0u8; 256];
    let Some(path_length) = common::resolve_path(envp, envc, argument, &mut path) else {
        common::write(b"vi: path too long\n");
        common::exit(2);
    };

    let mut editor = Editor::new(&path[..path_length]);
    let read_length = common::read_file(&path[..path_length], &mut editor.buffer);
    if read_length == -2 {
        editor.status = b"New file";
    } else if read_length < 0 {
        common::write(b"vi: cannot read file\n");
        common::exit(2);
    } else {
        editor.length = read_length as usize;
        if core::str::from_utf8(&editor.buffer[..editor.length]).is_err() {
            common::write(b"vi: file is not valid UTF-8\n");
            common::exit(2);
        }
    }

    loop {
        editor.render();
        let key = common::read_char();
        if editor.handle_key(key) {
            common::exit(0);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Insert,
    Command,
}

struct Editor<'a> {
    buffer: [u8; CAPACITY],
    length: usize,
    cursor: usize,
    path: &'a [u8],
    mode: Mode,
    command: [u8; 8],
    command_length: usize,
    dirty: bool,
    status: &'static [u8],
}

impl<'a> Editor<'a> {
    fn new(path: &'a [u8]) -> Self {
        Self {
            buffer: [0; CAPACITY],
            length: 0,
            cursor: 0,
            path,
            mode: Mode::Normal,
            command: [0; 8],
            command_length: 0,
            dirty: false,
            status: b"Ready",
        }
    }

    fn handle_key(&mut self, key: char) -> bool {
        match self.mode {
            Mode::Insert => self.handle_insert_key(key),
            Mode::Command => self.handle_command_key(key),
            Mode::Normal => self.handle_normal_key(key),
        }
    }

    fn handle_insert_key(&mut self, key: char) -> bool {
        match key {
            '\u{1b}' => self.mode = Mode::Normal,
            '\u{8}' | '\u{7f}' => self.backspace(),
            '\n' | '\r' => self.insert_bytes(b"\n"),
            KEY_UP => self.move_vertical(-1),
            KEY_DOWN => self.move_vertical(1),
            KEY_LEFT => self.move_left(),
            KEY_RIGHT => self.move_right(),
            KEY_HOME => self.move_home(),
            KEY_END => self.move_end(),
            KEY_DELETE => self.delete_at_cursor(),
            value if !value.is_control() && !is_keyboard_event(value) => {
                let mut encoded = [0u8; 4];
                self.insert_bytes(value.encode_utf8(&mut encoded).as_bytes());
            }
            _ => {}
        }
        false
    }

    fn handle_normal_key(&mut self, key: char) -> bool {
        match key {
            'i' => {
                self.mode = Mode::Insert;
                self.status = b"-- INSERT --";
            }
            'a' => {
                self.move_right();
                self.mode = Mode::Insert;
                self.status = b"-- INSERT --";
            }
            'h' | KEY_LEFT => self.move_left(),
            'l' | KEY_RIGHT => self.move_right(),
            'j' | KEY_DOWN => self.move_vertical(1),
            'k' | KEY_UP => self.move_vertical(-1),
            '0' | KEY_HOME => self.move_home(),
            '$' | KEY_END => self.move_end(),
            'x' | KEY_DELETE => self.delete_at_cursor(),
            'o' => {
                self.move_end();
                self.insert_bytes(b"\n");
                self.mode = Mode::Insert;
                self.status = b"-- INSERT --";
            }
            ':' => {
                self.command.fill(0);
                self.command_length = 0;
                self.mode = Mode::Command;
            }
            _ => {}
        }
        false
    }

    fn handle_command_key(&mut self, key: char) -> bool {
        match key {
            '\u{1b}' => self.mode = Mode::Normal,
            '\u{8}' | '\u{7f}' => {
                self.command_length = self.command_length.saturating_sub(1);
            }
            '\n' | '\r' => {
                let mut command = [0u8; 8];
                command[..self.command_length]
                    .copy_from_slice(&self.command[..self.command_length]);
                match &command[..self.command_length] {
                    b"w" => self.save(),
                    b"q" => {
                        if self.dirty {
                            self.status = b"Unsaved changes: use :q! or :w";
                        } else {
                            return true;
                        }
                    }
                    b"q!" => return true,
                    b"wq" => {
                        self.save();
                        if !self.dirty {
                            return true;
                        }
                    }
                    _ => self.status = b"Unknown command",
                }
                self.mode = Mode::Normal;
            }
            value if value.is_ascii() && !value.is_control() => {
                if self.command_length < self.command.len() {
                    self.command[self.command_length] = value as u8;
                    self.command_length += 1;
                }
            }
            _ => {}
        }
        false
    }

    fn save(&mut self) {
        let result = common::write_file(self.path, &self.buffer[..self.length]);
        if result == self.length as i64 {
            self.dirty = false;
            self.status = b"Saved to volatile RAMFS";
        } else {
            self.status = b"Write failed (RAMFS limit: 4 KiB)";
        }
    }

    fn insert_bytes(&mut self, bytes: &[u8]) {
        if self.length + bytes.len() > self.buffer.len() {
            self.status = b"Buffer full (4 KiB maximum)";
            return;
        }
        self.buffer
            .copy_within(self.cursor..self.length, self.cursor + bytes.len());
        self.buffer[self.cursor..self.cursor + bytes.len()].copy_from_slice(bytes);
        self.cursor += bytes.len();
        self.length += bytes.len();
        self.dirty = true;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let previous = previous_char_boundary(&self.buffer[..self.length], self.cursor);
        self.buffer.copy_within(self.cursor..self.length, previous);
        self.length -= self.cursor - previous;
        self.cursor = previous;
        self.dirty = true;
    }

    fn delete_at_cursor(&mut self) {
        if self.cursor == self.length {
            return;
        }
        let next = next_char_boundary(&self.buffer[..self.length], self.cursor);
        self.buffer.copy_within(next..self.length, self.cursor);
        self.length -= next - self.cursor;
        self.dirty = true;
    }

    fn move_left(&mut self) {
        self.cursor = previous_char_boundary(&self.buffer[..self.length], self.cursor);
    }

    fn move_right(&mut self) {
        self.cursor = next_char_boundary(&self.buffer[..self.length], self.cursor);
    }

    fn move_home(&mut self) {
        self.cursor = self.current_line_range().0;
    }

    fn move_end(&mut self) {
        self.cursor = self.current_line_range().1;
    }

    fn move_vertical(&mut self, direction: isize) {
        let (line_start, _) = self.current_line_range();
        let column = self.buffer[line_start..self.cursor]
            .iter()
            .filter(|byte| **byte & 0xc0 != 0x80)
            .count();
        let line = self.current_line();
        let target_line = if direction < 0 {
            line.checked_sub(direction.unsigned_abs())
        } else {
            line.checked_add(direction as usize)
        };
        let Some((target_start, target_end)) =
            target_line.and_then(|target| self.line_range(target))
        else {
            return;
        };
        self.cursor = byte_at_character_column(
            &self.buffer[target_start..target_end],
            column,
        ) + target_start;
    }

    fn current_line(&self) -> usize {
        self.buffer[..self.cursor]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
    }

    fn current_line_range(&self) -> (usize, usize) {
        self.line_range(self.current_line())
            .unwrap_or((0, self.length))
    }

    fn line_range(&self, wanted: usize) -> Option<(usize, usize)> {
        let mut start = 0usize;
        for _ in 0..wanted {
            let newline = self.buffer[start..self.length]
                .iter()
                .position(|byte| *byte == b'\n')?;
            start += newline + 1;
        }
        let end = self.buffer[start..self.length]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(self.length, |relative| start + relative);
        Some((start, end))
    }

    fn render(&self) {
        common::clear();
        common::write(b"vi ");
        common::write(self.path);
        common::write(match self.mode {
            Mode::Normal => b" [NORMAL]\n",
            Mode::Insert => b" [INSERT]\n",
            Mode::Command => b" :",
        });
        if self.mode == Mode::Command {
            common::write(&self.command[..self.command_length]);
            common::write(b"\n");
        }

        let current_line = self.current_line();
        let first_line = current_line.saturating_sub(VISIBLE_LINES / 2);
        for line in first_line..first_line + VISIBLE_LINES {
            let Some((start, end)) = self.line_range(line) else {
                break;
            };
            if line == current_line {
                self.render_current_line(start, end);
            } else {
                write_line(&self.buffer[start..end], 96);
            }
            common::write(b"\n");
        }
        common::write(self.status);
        if self.dirty {
            common::write(b" [+]");
        }
    }

    fn render_current_line(&self, start: usize, end: usize) {
        let cursor = self.cursor.clamp(start, end);
        let column = self.buffer[start..cursor]
            .iter()
            .filter(|byte| **byte & 0xc0 != 0x80)
            .count();
        let first_column = column.saturating_sub(48);
        let last_column = first_column + 96;
        let mut current_column = 0usize;
        for character in core::str::from_utf8(&self.buffer[start..end])
            .unwrap_or("")
            .chars()
        {
            if current_column == column {
                common::write(b"|");
            }
            if (first_column..last_column).contains(&current_column) {
                let mut encoded = [0u8; 4];
                common::write(character.encode_utf8(&mut encoded).as_bytes());
            }
            current_column += 1;
        }
        if current_column == column {
            common::write(b"|");
        }
    }
}

fn write_line(line: &[u8], max_chars: usize) {
    let text = core::str::from_utf8(line).unwrap_or("");
    let end = text
        .char_indices()
        .nth(max_chars)
        .map_or(text.len(), |(index, _)| index);
    common::write(&line[..end]);
}

fn byte_at_character_column(line: &[u8], wanted: usize) -> usize {
    core::str::from_utf8(line)
        .unwrap_or("")
        .char_indices()
        .nth(wanted)
        .map_or(line.len(), |(index, _)| index)
}

fn previous_char_boundary(bytes: &[u8], position: usize) -> usize {
    let mut previous = position.saturating_sub(1);
    while previous > 0 && bytes[previous] & 0xc0 == 0x80 {
        previous -= 1;
    }
    previous
}

fn next_char_boundary(bytes: &[u8], position: usize) -> usize {
    let mut next = (position + 1).min(bytes.len());
    while next < bytes.len() && bytes[next] & 0xc0 == 0x80 {
        next += 1;
    }
    next
}

fn is_keyboard_event(character: char) -> bool {
    ('\u{f0000}'..='\u{f0006}').contains(&character)
}
