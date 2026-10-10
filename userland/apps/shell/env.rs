//! Tabela de variáveis de ambiente do shell.
//!
//! Um array fixo de `Variable`, no mesmo espírito do resto do userspace: sem
//! alocador, com atualização no lugar. A tabela guarda tanto as variáveis
//! exportadas (que vão para `envp` dos filhos) quanto `$?` e `$0`.

pub const MAX_VARIABLES: usize = 24;
pub const NAME_SIZE: usize = 24;
pub const VALUE_SIZE: usize = 96;

#[derive(Clone, Copy)]
pub struct Variable {
    pub name: [u8; NAME_SIZE],
    pub name_length: usize,
    pub value: [u8; VALUE_SIZE],
    pub value_length: usize,
    pub active: bool,
    /// Exportada para o ambiente dos processos filhos.
    pub exported: bool,
}

impl Variable {
    const EMPTY: Self = Self {
        name: [0; NAME_SIZE],
        name_length: 0,
        value: [0; VALUE_SIZE],
        value_length: 0,
        active: false,
        exported: false,
    };
}

/// Tamanho de um par `NAME=VALUE`, incluindo o `=` e o terminador implícito.
pub const PAIR_SIZE: usize = NAME_SIZE + VALUE_SIZE + 1;

/// Texto `NAME=VALUE` de uma variável exportada, materializado pelo chamador.
///
/// Reutilizável: cada `spawn` escreve nesta estrutura e depois a usa como
/// `envp`. Manter os buffers fora de [`Environment`] evita auto-referência
/// (um campo do struct tomando emprestado outro), que o `no_std` não permite
/// expressar de forma limpa.
pub struct EnvironmentPairs {
    storage: [[u8; PAIR_SIZE]; MAX_VARIABLES],
    lengths: [usize; MAX_VARIABLES],
    count: usize,
}

impl EnvironmentPairs {
    pub const fn new() -> Self {
        Self {
            storage: [[0; PAIR_SIZE]; MAX_VARIABLES],
            lengths: [0; MAX_VARIABLES],
            count: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.count
    }

    /// Empresta os pares prontos como `&str`, para uso direto em `envp`.
    pub fn as_strs<'b>(&'b self) -> [&'b str; MAX_VARIABLES] {
        let mut texts = [""; MAX_VARIABLES];
        for index in 0..self.count {
            texts[index] =
                core::str::from_utf8(&self.storage[index][..self.lengths[index]]).unwrap_or("");
        }
        texts
    }
}

/// A tabela de variáveis, com acesso auxiliar por nome.
pub struct Environment {
    variables: [Variable; MAX_VARIABLES],
}

impl Environment {
    pub fn new() -> Self {
        let mut environment = Self {
            variables: [Variable::EMPTY; MAX_VARIABLES],
        };
        environment.set(b"HOME", b"/home", true);
        environment.set(b"PATH", b"/bin", true);
        environment.set(b"PWD", b"/", true);
        environment.set(b"SHELL", b"/bin/shell", true);
        environment.set(b"?", b"0", false);
        environment
    }

    pub fn get(&self, name: &[u8]) -> Option<&[u8]> {
        self.variables
            .iter()
            .find(|variable| variable.active && variable.name[..variable.name_length] == *name)
            .map(|variable| &variable.value[..variable.value_length])
    }

    pub fn set(&mut self, name: &[u8], value: &[u8], exported: bool) -> bool {
        if !valid_name(name) || name.len() > NAME_SIZE || value.len() > VALUE_SIZE {
            return false;
        }
        let slot = self
            .variables
            .iter()
            .position(|variable| variable.active && variable.name[..variable.name_length] == *name)
            .or_else(|| self.variables.iter().position(|variable| !variable.active));
        let Some(slot) = slot else {
            return false;
        };
        let variable = &mut self.variables[slot];
        *variable = Variable::EMPTY;
        variable.name[..name.len()].copy_from_slice(name);
        variable.value[..value.len()].copy_from_slice(value);
        variable.name_length = name.len();
        variable.value_length = value.len();
        variable.active = true;
        variable.exported = exported;
        true
    }

    /// Atualiza `$?` com o código de saída do último comando.
    ///
    /// O valor é truncado a 8 bits como no POSIX: um processo que retorna 256
    /// aparece como 0 para o shell.
    pub fn set_status(&mut self, status: i64) {
        let mut buffer = [0u8; 24];
        let byte = status as u8;
        let length = format_decimal(u64::from(byte), &mut buffer);
        let _ = self.set(b"?", &buffer[..length], true);
    }

    pub fn unset(&mut self, name: &[u8]) {
        if let Some(variable) = self
            .variables
            .iter_mut()
            .find(|variable| variable.active && variable.name[..variable.name_length] == *name)
        {
            *variable = Variable::EMPTY;
        }
    }

    pub fn iter_exported(&self) -> impl Iterator<Item = &Variable> {
        self.variables
            .iter()
            .filter(|variable| variable.active && variable.exported)
    }

    /// Materializa as variáveis exportadas em `NAME=VALUE` dentro de `pairs`.
    pub fn fill_pairs(&self, pairs: &mut EnvironmentPairs) {
        let mut count = 0usize;
        for variable in self.iter_exported() {
            if count == MAX_VARIABLES {
                break;
            }
            let name_end = variable.name_length;
            let value_end = name_end + 1 + variable.value_length;
            if value_end > PAIR_SIZE {
                continue;
            }
            let pair = &mut pairs.storage[count];
            pair[..name_end].copy_from_slice(&variable.name[..name_end]);
            pair[name_end] = b'=';
            pair[name_end + 1..value_end].copy_from_slice(&variable.value[..variable.value_length]);
            pairs.lengths[count] = value_end;
            count += 1;
        }
        pairs.count = count;
    }
}

pub fn valid_name(name: &[u8]) -> bool {
    match name.first() {
        Some(first) if first.is_ascii_alphabetic() || *first == b'_' || *first == b'?' => {}
        _ => return false,
    }
    name[1..]
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

fn format_decimal(mut value: u64, output: &mut [u8; 24]) -> usize {
    let mut digits = [0u8; 20];
    let mut cursor = digits.len();
    if value == 0 {
        output[0] = b'0';
        return 1;
    }
    while value != 0 {
        cursor -= 1;
        digits[cursor] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    let length = digits.len() - cursor;
    output[..length].copy_from_slice(&digits[cursor..]);
    length
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_gets_and_unsets() {
        let mut environment = Environment::new();
        assert_eq!(environment.get(b"PATH"), Some(&b"/bin"[..]));
        assert!(environment.set(b"FOO", b"bar", true));
        assert_eq!(environment.get(b"FOO"), Some(&b"bar"[..]));
        environment.unset(b"FOO");
        assert_eq!(environment.get(b"FOO"), None);
    }

    #[test]
    fn rejects_invalid_names() {
        let mut environment = Environment::new();
        assert!(!environment.set(b"1BAD", b"x", true));
        assert!(!environment.set(b"", b"x", true));
        assert!(!environment.set(&[b'a'; NAME_SIZE + 1], b"x", true));
    }

    #[test]
    fn only_exported_variables_become_envp_pairs() {
        let mut environment = Environment::new();
        environment.set(b"SECRET", b"x", false);
        let mut pairs = EnvironmentPairs::new();
        environment.fill_pairs(&mut pairs);
        let texts = pairs.as_strs();
        let ready = &texts[..pairs.len()];
        assert!(ready.iter().any(|pair| pair.starts_with("PATH=")));
        assert!(!ready.iter().any(|pair| pair.starts_with("SECRET=")));
    }
}
