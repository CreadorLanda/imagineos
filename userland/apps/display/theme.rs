//! Engine de temas do window manager.
//!
//! O WM olha para o tema em `system/themes/default.theme`, arquivo
//! `window/windowTheme.css`. A aparência das janelas é um subconjunto de CSS
//! interpretado por um parser embutido e renderizado com primitivas gráficas
//! (retângulos e pixels) — sem bitmaps, como os toolkits clássicos:
//!
//! ```css
//! /* comentário */
//! #titlebar.focused {
//!     background-color: #176b87;
//!     color: #ffffff;
//!     height: 18px;
//! }
//! #button.close  { background-color: #c94c4c; }
//! #border        { border-width: 2px; border-color: #0b1118; }
//! #cursor        { background-color: #ffffff; color: #101820; }
//! ```
//!
//! Propriedades aceitas: `background-color`/`background`, `color`,
//! `border-color`, `height`, `width`, `size`, `border-width`/`border`.
//! Valores: `#rrggbb`, `#rgb`, `white`, `black` e `Npx`. Tudo o que faltar
//! mantém o padrão embutido — o tema nunca quebra o WM.
//!
//! O parser não aloca e é independente do kernel, então roda nos testes de
//! host com o arquivo real de `ramfs/`.

use imagineos::fs;

/// Diretório raiz do tema ativo.
pub const THEME_PATH: &str = "system/themes/default.theme";
const THEME_PATH_MAX: usize = 96;
/// Tamanho de leitura da syscall `read_file` (limite do kernel).
const READ_FILE_CHUNK: usize = 4096;
/// Teto para o arquivo de tema individual.
const MAX_FILE_SIZE: usize = 64 * 1024;
/// Tamanho máximo do retângulo de cursor salvo/restaurado por frame.
pub const MAX_CURSOR_WIDTH: usize = 32;
pub const MAX_CURSOR_HEIGHT: usize = 32;

/// Cores/dimensões das janelas, derivadas do CSS com fallbacks embutidos.
#[derive(Clone, Copy)]
pub struct WindowTheme {
    pub desktop: u32,
    pub desktop_accent: u32,
    pub window: u32,
    pub window_alt: u32,
    pub title_active: u32,
    pub title_inactive: u32,
    pub title_text: u32,
    pub border_color: u32,
    pub button_close: u32,
    pub button_minimize: u32,
    pub cursor_color: u32,
    pub cursor_outline: u32,
    pub border: i32,
    pub title_height: i32,
    pub button_size: i32,
    pub cursor_size: i32,
}

pub const DEFAULTS: WindowTheme = WindowTheme {
    desktop: 0x17212b,
    desktop_accent: 0x1f6f78,
    window: 0xe8edf2,
    window_alt: 0xd8e0e8,
    title_active: 0x176b87,
    title_inactive: 0x465563,
    title_text: 0xffffff,
    border_color: 0x0b1118,
    button_close: 0xc94c4c,
    button_minimize: 0xe0ad4f,
    cursor_color: 0xffffff,
    cursor_outline: 0x101820,
    border: 2,
    title_height: 18,
    button_size: 12,
    cursor_size: 10,
};

/// Tema completo carregado do disco.
#[derive(Clone, Copy)]
pub struct Theme {
    pub window: WindowTheme,
}

impl Theme {
    /// Tema embutido, usado quando o disco não tem tema (boot mínimo).
    pub const fn builtin() -> Self {
        Self { window: DEFAULTS }
    }

    /// Carrega o tema do disco. Falha no CSS degrada para os padrões: o WM
    /// nunca deixa de subir por causa de um tema ruim.
    pub fn load() -> Self {
        let mut window = DEFAULTS;
        let mut css = [0u8; MAX_FILE_SIZE];
        if let Some(length) = read_theme_file("window/windowTheme.css", &mut css) {
            apply_css(&mut window, &css[..length]);
        }
        Self { window }
    }
}

/// Lê um arquivo dentro do diretório do tema para `output`, devolvendo o
/// comprimento lido. `fs::read_file` transfere até 4 KiB por chamada, então
/// arquivos maiores são completados com leituras por descritor.
fn read_theme_file(relative: &str, output: &mut [u8]) -> Option<usize> {
    let mut path = [0u8; THEME_PATH_MAX];
    let mut length = THEME_PATH.len();
    path[..length].copy_from_slice(THEME_PATH.as_bytes());
    if length + 1 + relative.len() >= THEME_PATH_MAX {
        return None;
    }
    path[length] = b'/';
    length += 1;
    path[length..length + relative.len()].copy_from_slice(relative.as_bytes());
    length += relative.len();
    let path = core::str::from_utf8(&path[..length]).ok()?;

    // Leitura direta: cobre o CSS (que é pequeno).
    if output.len() >= READ_FILE_CHUNK {
        if let Ok(read) = fs::read_file(path, &mut output[..READ_FILE_CHUNK]) {
            if read < READ_FILE_CHUNK {
                return Some(read);
            }
            // Arquivo de 4 KiB ou mais: reabre e completa por descritor.
            let mut total = read;
            let file = fs::open(path, fs::OpenOptions::new().read(true)).ok()?;
            let fd = file.descriptor();
            while total < output.len() {
                let chunk = READ_FILE_CHUNK.min(output.len() - total);
                match fs::read(fd, &mut output[total..total + chunk]) {
                    Ok(read) if read > 0 => total += read,
                    _ => break,
                }
            }
            let _ = fs::close(fd);
            if total > 0 {
                return Some(total);
            }
        }
    }
    None
}

// ----------------------------------------------------------------------------
// Parser do CSS
// ----------------------------------------------------------------------------

/// Aplica o conteúdo de um CSS ao tema.
pub fn apply_css(theme: &mut WindowTheme, source: &[u8]) {
    // Duas passadas: primeiro as regras genéricas (`#titlebar`), depois as
    // específicas (`#titlebar.focused`). Assim a especificidade vence
    // independentemente da ordem no arquivo, como no CSS real.
    apply_css_pass(theme, source, false);
    apply_css_pass(theme, source, true);
}

fn apply_css_pass(theme: &mut WindowTheme, source: &[u8], specific: bool) {
    let mut cursor = 0;
    while cursor < source.len() {
        skip_whitespace_and_comments(source, &mut cursor);
        if cursor >= source.len() || source[cursor] != b'#' {
            // Regra sem seletor `#` (ou lixo): pula até o fim do bloco.
            while cursor < source.len() && source[cursor] != b'{' {
                cursor += 1;
            }
            skip_block(source, &mut cursor);
            continue;
        }
        let selector_start = cursor;
        while cursor < source.len() && source[cursor] != b'{' {
            cursor += 1;
        }
        if cursor >= source.len() {
            return;
        }
        let selector = trim(&source[selector_start..cursor]);
        // Nesta passada só processamos os seletores do tipo correspondente.
        let has_variant = selector[1..].contains(&b'.');
        if has_variant != specific {
            cursor += 1;
            skip_block(source, &mut cursor);
            continue;
        }
        cursor += 1; // '{'
        let body_start = cursor;
        skip_block(source, &mut cursor);
        let body_end = cursor.saturating_sub(1).max(body_start);
        let body = &source[body_start..body_end];
        apply_rule(theme, selector, body);
    }
}

/// Avança `cursor` para depois do `}` do bloco aberto em `cursor - 1`.
fn skip_block(source: &[u8], cursor: &mut usize) {
    while *cursor < source.len() && source[*cursor] != b'}' {
        *cursor += 1;
    }
    if *cursor < source.len() {
        *cursor += 1;
    }
}

fn skip_whitespace_and_comments(source: &[u8], cursor: &mut usize) {
    loop {
        while *cursor < source.len() && source[*cursor].is_ascii_whitespace() {
            *cursor += 1;
        }
        if *cursor + 1 < source.len() && source[*cursor] == b'/' && source[*cursor + 1] == b'*' {
            *cursor += 2;
            while *cursor + 1 < source.len()
                && !(source[*cursor] == b'*' && source[*cursor + 1] == b'/')
            {
                *cursor += 1;
            }
            *cursor = (*cursor + 2).min(source.len());
            continue;
        }
        return;
    }
}

fn trim(bytes: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = bytes.len();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &bytes[start..end]
}

/// Aplica um bloco `propriedade: valor;` a um seletor.
fn apply_rule(theme: &mut WindowTheme, selector: &[u8], body: &[u8]) {
    let mut cursor = 0;
    while cursor < body.len() {
        while cursor < body.len() && body[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let name_start = cursor;
        while cursor < body.len() && body[cursor] != b':' {
            cursor += 1;
        }
        if cursor >= body.len() {
            return;
        }
        let name = trim(&body[name_start..cursor]);
        cursor += 1;
        let value_start = cursor;
        while cursor < body.len() && body[cursor] != b';' && body[cursor] != b'}' {
            cursor += 1;
        }
        let value = trim(&body[value_start..cursor]);
        cursor += 1;
        set_property(theme, selector, name, value);
    }
}

/// Compara nomes em minúsculas (`BACKGROUND-COLOR` casa com `background-color`).
fn name_is(name: &[u8], expected: &str) -> bool {
    name.len() == expected.len()
        && name
            .iter()
            .zip(expected.as_bytes())
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
}

/// O seletor casa com `#base[.modificador]`? Um seletor sem modificador
/// (`#titlebar`) vale para todas as variantes; um com modificador
/// (`#titlebar.focused`) só para a variante pedida.
fn selector_matches(selector: &[u8], base: &str, modifier: Option<&str>) -> bool {
    if selector.is_empty() || selector[0] != b'#' {
        return false;
    }
    let selector = &selector[1..];
    let dot = selector.iter().position(|&b| b == b'.');
    let (name, variant) = match dot {
        Some(dot) => (&selector[..dot], Some(&selector[dot + 1..])),
        None => (selector, None),
    };
    if !name_is(name, base) {
        return false;
    }
    match (variant, modifier) {
        (None, _) => true,
        (Some(variant), Some(wanted)) => name_is(variant, wanted),
        (Some(_), None) => false,
    }
}

/// Trata uma propriedade de um bloco. O mesmo conjunto de propriedades vale
/// para todos os seletores; o que muda é para qual cor/dimensão ela aponta.
fn set_property(theme: &mut WindowTheme, selector: &[u8], name: &[u8], value: &[u8]) {
    let color = if name_is(name, "background-color")
        || name_is(name, "background")
        || name_is(name, "border-color")
        || name_is(name, "color")
    {
        parse_color(value)
    } else {
        None
    };
    let dimension = if name_is(name, "height")
        || name_is(name, "width")
        || name_is(name, "size")
        || name_is(name, "border-width")
        || name_is(name, "border")
    {
        parse_px(value)
    } else {
        None
    };
    let Some(color) = color else {
        if let Some(dimension) = dimension {
            set_dimension(theme, selector, dimension);
        }
        return;
    };
    // Regras genéricas já rodaram na primeira passada; nesta (específica)
    // apenas as variantes casam, então a especificidade é respeitada.
    if selector_matches(selector, "titlebar", Some("focused")) {
        if name_is(name, "color") {
            theme.title_text = color;
        } else {
            theme.title_active = color;
        }
        return;
    }
    if selector_matches(selector, "titlebar", Some("unfocused")) {
        theme.title_inactive = color;
        return;
    }
    if selector_matches(selector, "button", Some("close")) {
        theme.button_close = color;
        return;
    }
    if selector_matches(selector, "button", Some("minimize")) {
        theme.button_minimize = color;
        return;
    }
    if selector_matches(selector, "cursor", Some("outline")) {
        theme.cursor_outline = color;
        return;
    }
    if selector_matches(selector, "desktop", None) {
        theme.desktop = color;
    } else if selector_matches(selector, "window", None) {
        theme.window = color;
    } else if selector_matches(selector, "titlebar", None) {
        if name_is(name, "color") {
            theme.title_text = color;
        } else {
            theme.title_active = color;
        }
    } else if selector_matches(selector, "button", None) {
        theme.button_close = color;
    } else if selector_matches(selector, "border", None) {
        theme.border_color = color;
    } else if selector_matches(selector, "cursor", None) {
        if name_is(name, "color") || name_is(name, "border-color") {
            theme.cursor_outline = color;
        } else {
            theme.cursor_color = color;
        }
    }
    if let Some(dimension) = dimension {
        set_dimension(theme, selector, dimension);
    }
}

fn set_dimension(theme: &mut WindowTheme, selector: &[u8], value: i32) {
    if value <= 0 {
        return;
    }
    if selector_matches(selector, "titlebar", None)
        || selector_matches(selector, "titlebar", Some("focused"))
        || selector_matches(selector, "titlebar", Some("unfocused"))
    {
        theme.title_height = value;
    } else if selector_matches(selector, "button", None)
        || selector_matches(selector, "button", Some("close"))
    {
        theme.button_size = value;
    } else if selector_matches(selector, "border", None) {
        theme.border = value;
    } else if selector_matches(selector, "cursor", None) {
        theme.cursor_size = value;
    }
}

/// `#rrggbb` (ou `#rgb`), além dos nomes `white`/`black`.
pub fn parse_color(value: &[u8]) -> Option<u32> {
    let value = trim(value);
    if value.first() == Some(&b'#') {
        let hex = &value[1..];
        if hex.len() == 6 {
            let red = parse_hex_pair(&hex[0..2])?;
            let green = parse_hex_pair(&hex[2..4])?;
            let blue = parse_hex_pair(&hex[4..6])?;
            return Some(((red as u32) << 16) | ((green as u32) << 8) | blue as u32);
        }
        if hex.len() == 3 {
            let mut channels = [0u32; 3];
            for (index, byte) in hex.iter().take(3).enumerate() {
                let digit = (*byte as char).to_digit(16)?;
                channels[index] = digit * 16 + digit;
            }
            return Some((channels[0] << 16) | (channels[1] << 8) | channels[2]);
        }
        return None;
    }
    if name_is(value, "white") {
        return Some(0xffffff);
    }
    if name_is(value, "black") {
        return Some(0x000000);
    }
    None
}

fn parse_hex_pair(bytes: &[u8]) -> Option<u8> {
    let high = (bytes[0] as char).to_digit(16)?;
    let low = (bytes[1] as char).to_digit(16)?;
    Some((high * 16 + low) as u8)
}

/// `18px` → 18; números sem sufixo também são aceitos.
pub fn parse_px(value: &[u8]) -> Option<i32> {
    let value = trim(value);
    let digits = if value.len() >= 2 && value[value.len() - 2..] == *b"px" {
        trim(&value[..value.len() - 2])
    } else {
        trim(value)
    };
    if digits.is_empty() || !digits.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut number: i32 = 0;
    for &digit in digits {
        number = number
            .saturating_mul(10)
            .saturating_add((digit - b'0') as i32);
    }
    Some(number)
}

// ----------------------------------------------------------------------------
// Testes de host: rodam com o arquivo real do tema em ramfs/
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_css_from_default_theme() {
        let css = core::str::from_utf8(include_bytes!(
            "../../../ramfs/system/themes/default.theme/window/windowTheme.css"
        ))
        .unwrap();
        let mut theme = DEFAULTS;
        apply_css(&mut theme, css.as_bytes());
        // Dimensões sempre positivas depois do parse.
        assert!(theme.title_height > 0);
        assert!(theme.button_size > 0);
        assert!(theme.border >= 0);
    }

    #[test]
    fn css_specificity_and_fallbacks() {
        let mut theme = DEFAULTS;
        apply_css(
            &mut theme,
            b"/* comment */ #titlebar { background-color: #445566; height: 24px; }\n\
              #titlebar.focused { background-color: #112233; }\n\
              #button.close { background-color: #ff0000; }\n\
              #border { border-width: 4px; border-color: #010203; }\n\
              #desktop { background-color: #000000; }",
        );
        // A variante específica vence independentemente da ordem.
        assert_eq!(theme.title_active, 0x112233);
        assert_eq!(theme.title_height, 24);
        assert_eq!(theme.button_close, 0xff0000);
        assert_eq!(theme.border, 4);
        assert_eq!(theme.border_color, 0x010203);
        assert_eq!(theme.desktop, 0x000000);
        // Tudo o que não foi declarado mantém o padrão:
        assert_eq!(theme.window, DEFAULTS.window);
        assert_eq!(theme.title_inactive, DEFAULTS.title_inactive);
    }

    #[test]
    fn parses_colors_and_px() {
        assert_eq!(parse_color(b"#176b87"), Some(0x176b87));
        assert_eq!(parse_color(b"#FFF"), Some(0xffffff));
        assert_eq!(parse_color(b"white"), Some(0xffffff));
        assert_eq!(parse_color(b"rgb(1,2,3)"), None);
        assert_eq!(parse_px(b"18px"), Some(18));
        assert_eq!(parse_px(b"18"), Some(18));
        assert_eq!(parse_px(b"-2px"), None);
        assert_eq!(parse_px(b"auto"), None);
    }
}
