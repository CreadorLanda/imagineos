#![no_std]
#![no_main]

#[allow(dead_code)]
mod common;

const CONFIG_PATH: &[u8] = b"/home/.global/global.conf";
const DEFAULT_CONFIG: &[u8] = b"charset=utf-8\nkeyboard=abnt2\n";
const MAX_CONFIG_SIZE: usize = 4096;

#[unsafe(no_mangle)]
extern "C" fn _start(
    argc: usize,
    argv: *const *const u8,
    _envc: usize,
    _envp: *const *const u8,
) -> ! {
    let mut current = [0u8; MAX_CONFIG_SIZE];
    let length = common::read_file(CONFIG_PATH, &mut current);
    if length < 0 {
        if length != -2
            || argc == 1
            || unsafe { common::argument(argv, 1) } != Some(&b"set"[..])
        {
            common::write(b"globalconf: cannot read /home/.global/global.conf\n");
            common::exit(1);
        }
        current[..DEFAULT_CONFIG.len()].copy_from_slice(DEFAULT_CONFIG);
    }
    let mut length = if length < 0 {
        DEFAULT_CONFIG.len()
    } else {
        length as usize
    };

    if argc == 1 {
        common::write(&current[..length]);
        common::exit(0);
    }

    let Some(action) = (unsafe { common::argument(argv, 1) }) else {
        usage();
    };
    match action {
        b"show" | b"list" => {
            common::write(&current[..length]);
            common::exit(0);
        }
        b"get" if argc == 3 => {
            let Some(key) = (unsafe { common::argument(argv, 2) }) else {
                common::exit(2);
            };
            for line in current[..length].split(|byte| *byte == b'\n') {
                if let Some((found_key, value)) = split_setting(line) {
                    if found_key == key {
                        common::write(value);
                        common::write(b"\n");
                        common::exit(0);
                    }
                }
            }
            common::write(b"globalconf: setting not found\n");
            common::exit(1);
        }
        b"set" if argc == 4 => {
            let Some(key) = (unsafe { common::argument(argv, 2) }) else {
                common::exit(2);
            };
            let Some(value) = (unsafe { common::argument(argv, 3) }) else {
                common::exit(2);
            };
            if !valid_key(key) || !valid_value(value) {
                common::write(b"globalconf: invalid key or value\n");
                common::exit(2);
            }
            let mut updated = [0u8; MAX_CONFIG_SIZE];
            let Some(updated_length) =
                update_setting(&current[..length], key, value, &mut updated)
            else {
                common::write(b"globalconf: configuration is too large\n");
                common::exit(1);
            };
            length = updated_length;
            let result = common::write_file(CONFIG_PATH, &updated[..length]);
            if result == -22 {
                common::write(
                    b"globalconf: invalid setting; supported values: charset=utf-8, keyboard=us|abnt2\n",
                );
                common::exit(2);
            }
            if result < 0 {
                common::write(b"globalconf: cannot save configuration\n");
                common::exit(1);
            }
            common::write(b"Configuration updated for this boot session.\n");
            common::exit(0);
        }
        _ => usage(),
    }
}

fn usage() -> ! {
    common::write(
        b"usage: globalconf [show | get KEY | set KEY VALUE]\n\
settings: charset=utf-8, keyboard=us|abnt2\n",
    );
    common::exit(2)
}

fn split_setting(line: &[u8]) -> Option<(&[u8], &[u8])> {
    if line.starts_with(b"#") {
        return None;
    }
    let separator = line.iter().position(|byte| *byte == b'=')?;
    Some((trim(&line[..separator]), trim(&line[separator + 1..])))
}

fn trim(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn valid_key(key: &[u8]) -> bool {
    !key.is_empty()
        && key
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'.')
}

fn valid_value(value: &[u8]) -> bool {
    !value.is_empty() && value.iter().all(|byte| *byte != b'\n' && *byte != b'\r')
}

fn update_setting(
    source: &[u8],
    key: &[u8],
    value: &[u8],
    output: &mut [u8],
) -> Option<usize> {
    let mut written = 0usize;
    let mut found = false;
    for line in source.split_inclusive(|byte| *byte == b'\n') {
        let same_key = split_setting(trim(line))
            .is_some_and(|(existing, _)| existing == key);
        if same_key {
            if !found {
                written = append_setting(output, written, key, value)?;
                found = true;
            }
        } else {
            let end = written.checked_add(line.len())?;
            if end > output.len() {
                return None;
            }
            output[written..end].copy_from_slice(line);
            written = end;
        }
    }
    if !found {
        written = append_setting(output, written, key, value)?;
    }
    Some(written)
}

fn append_setting(output: &mut [u8], written: usize, key: &[u8], value: &[u8]) -> Option<usize> {
    let separator = usize::from(written > 0 && output[written - 1] != b'\n');
    let end = written.checked_add(separator)?.checked_add(key.len())?.checked_add(1)?
        .checked_add(value.len())?.checked_add(1)?;
    if end > output.len() {
        return None;
    }
    let mut cursor = written;
    if separator != 0 {
        output[cursor] = b'\n';
        cursor += 1;
    }
    output[cursor..cursor + key.len()].copy_from_slice(key);
    cursor += key.len();
    output[cursor] = b'=';
    cursor += 1;
    output[cursor..cursor + value.len()].copy_from_slice(value);
    cursor += value.len();
    output[cursor] = b'\n';
    Some(cursor + 1)
}

#[cfg(test)]
mod tests {
    use super::update_setting;

    #[test]
    fn updates_one_value_and_preserves_other_settings() {
        let mut output = [0u8; 128];
        let length =
            update_setting(b"# kept\ncharset=utf-8\nkeyboard=us\n", b"keyboard", b"abnt2", &mut output)
                .unwrap();
        assert_eq!(&output[..length], b"# kept\ncharset=utf-8\nkeyboard=abnt2\n");
    }

    #[test]
    fn adds_missing_settings_to_empty_file() {
        let mut output = [0u8; 64];
        let length = update_setting(b"", b"keyboard", b"abnt2", &mut output).unwrap();
        assert_eq!(&output[..length], b"keyboard=abnt2\n");
    }
}
