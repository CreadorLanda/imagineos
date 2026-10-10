//! Resolução e normalização de caminhos.
//!
//! Nada aqui toca o filesystem: são funções puras sobre bytes, para que o shell
//! possa transformar `~/x`, `../y` ou `a/b` em um caminho absoluto antes de
//! consultar o kernel. As funções evitam alocação e escrevem em buffers do
//! chamador.

/// Tamanho máximo de um caminho manipulado pelo shell.
pub const MAX_PATH: usize = 256;
/// Profundidade máxima suportada ao normalizar (`..` é contado aqui).
pub const MAX_COMPONENTS: usize = 48;

/// Junta `cwd` + `path` e resolve `.`, `..` e barras repetidas.
///
/// Devolve o comprimento escrito em `output`, ou `None` se o resultado não
/// couber. Um `path` absoluto ignora `cwd`.
pub fn resolve(cwd: &[u8], path: &[u8], output: &mut [u8; MAX_PATH]) -> Option<usize> {
    let mut joined = [0u8; MAX_PATH * 2];
    let joined_length = if path.starts_with(b"/") {
        if path.len() > joined.len() {
            return None;
        }
        joined[..path.len()].copy_from_slice(path);
        path.len()
    } else {
        let mut length = cwd.len();
        if length > joined.len() {
            return None;
        }
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

    normalize(&joined[..joined_length], output)
}

/// Remove `.`, aplica `..` e garante um caminho absoluto sem barra final.
pub fn normalize(path: &[u8], output: &mut [u8; MAX_PATH]) -> Option<usize> {
    output[0] = b'/';
    let mut output_length = 1usize;
    // Onde cada componente começa, para podermos voltar atrás em `..`.
    let mut starts = [0usize; MAX_COMPONENTS];
    let mut count = 0usize;
    let mut position = 0usize;

    while position < path.len() {
        while position < path.len() && path[position] == b'/' {
            position += 1;
        }
        let start = position;
        while position < path.len() && path[position] != b'/' {
            position += 1;
        }
        let component = &path[start..position];
        if component.is_empty() || component == b"." {
            continue;
        }
        if component == b".." {
            if count > 0 {
                count -= 1;
                output_length = starts[count];
            }
            continue;
        }
        if count == MAX_COMPONENTS {
            return None;
        }
        if output_length > 1 {
            if output_length == output.len() {
                return None;
            }
            output[output_length] = b'/';
            output_length += 1;
        }
        starts[count] = output_length.saturating_sub(1).max(1);
        if output_length + component.len() > output.len() {
            return None;
        }
        output[output_length..output_length + component.len()].copy_from_slice(component);
        output_length += component.len();
        count += 1;
    }
    Some(output_length)
}

/// Expande um `~` inicial para `home`, escrevendo o resultado em `output`.
///
/// Devolve o comprimento escrito. Um caminho sem `~` é copiado como está, de
/// modo que o chamador pode usar sempre esta função.
pub fn expand_tilde(path: &[u8], home: &[u8], output: &mut [u8; MAX_PATH]) -> Option<usize> {
    let expanded = if path == b"~" {
        home
    } else if let Some(rest) = path.strip_prefix(b"~/") {
        // home + '/' + rest
        let mut length = home.len();
        if length >= output.len() {
            return None;
        }
        output[..length].copy_from_slice(home);
        if length == 0 || output[length - 1] != b'/' {
            output[length] = b'/';
            length += 1;
        }
        if length + rest.len() > output.len() {
            return None;
        }
        output[length..length + rest.len()].copy_from_slice(rest);
        return Some(length + rest.len());
    } else {
        path
    };
    if expanded.len() > output.len() {
        return None;
    }
    output[..expanded.len()].copy_from_slice(expanded);
    Some(expanded.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve_into(cwd: &str, path: &str) -> [u8; MAX_PATH] {
        let mut output = [0u8; MAX_PATH];
        let length = resolve(cwd.as_bytes(), path.as_bytes(), &mut output).expect("resolve");
        output[length] = 0;
        output
    }

    fn assert_resolves(cwd: &str, path: &str, expected: &str) {
        let output = resolve_into(cwd, path);
        let length = output.iter().position(|byte| *byte == 0).unwrap();
        assert_eq!(&output[..length], expected.as_bytes());
    }

    #[test]
    fn absolute_path_is_normalised() {
        assert_resolves("/any", "/a//b/./c", "/a/b/c");
    }

    #[test]
    fn relative_path_is_joined_with_cwd() {
        assert_resolves("/home", "sub/file", "/home/sub/file");
    }

    #[test]
    fn dot_dot_walks_up() {
        assert_resolves("/a/b/c", "../../x", "/a/x");
        assert_resolves("/", "../../x", "/x");
    }

    #[test]
    fn root_stays_root() {
        assert_resolves("/", "/", "/");
        assert_resolves("/a", "..", "/");
    }
}
