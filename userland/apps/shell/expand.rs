//! Expansão de palavras: variáveis (`$VAR`, `${VAR}`, `$?`) e concatenação de
//! segmentos.
//!
//! Regras seguidas (subconjunto fiel do POSIX):
//!
//! * `'...'` — nada é expandido;
//! * `"..."` — `$VAR` expande, mas o resultado não é dividido em palavras;
//! * texto sem aspas — `$VAR` expande e o resultado é dividido por espaço,
//!   produzindo vários argumentos (word splitting);
//! * `$$` — PID do shell; `$?` — status do último comando.

use crate::env::Environment;
use crate::lexer::{Segment, SegmentKind, Word};

/// Resultado da expansão de uma word: pode gerar mais de um argumento.
///
/// `MAX_FIELDS` e `FIELD_SIZE` são deliberadamente pequenos: `ExpandedWord` é
/// criado dentro de laços no executor, e a pilha do shell tem apenas 32 KiB.
/// Um campo de 64 bytes cobre caminhos curtos e nomes usuais.
pub const MAX_FIELDS: usize = 8;
pub const FIELD_SIZE: usize = 64;

pub struct ExpandedWord {
    fields: [[u8; FIELD_SIZE]; MAX_FIELDS],
    lengths: [u8; MAX_FIELDS],
    count: usize,
}

impl ExpandedWord {
    pub const fn new() -> Self {
        Self {
            fields: [[0; FIELD_SIZE]; MAX_FIELDS],
            lengths: [0; MAX_FIELDS],
            count: 0,
        }
    }

    pub fn as_slice(&self, index: usize) -> &[u8] {
        &self.fields[index][..usize::from(self.lengths[index])]
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Anexa um campo literal, usado por builtins que já têm o texto pronto
    /// (como `cd` com `$HOME`).
    pub fn push_field(&mut self, bytes: &[u8]) -> Result<(), ()> {
        if self.count == MAX_FIELDS || bytes.len() > FIELD_SIZE {
            return Err(());
        }
        let slot = self.count;
        self.fields[slot][..bytes.len()].copy_from_slice(bytes);
        self.lengths[slot] = bytes.len() as u8;
        self.count += 1;
        Ok(())
    }
}

/// Concatena os segmentos de `word`, aplicando expansão conforme o tipo.
///
/// Palavras sem variáveis e sem split produzem um único campo. Se houver
/// split, cada sequência separada por espaço vira um campo.
pub fn expand(word: &Word, environment: &Environment, output: &mut ExpandedWord) {
    output.count = 0;
    let mut current = [0u8; FIELD_SIZE];
    let mut length = 0usize;
    let mut any_produced = false;

    for segment in word.segments[..word.segment_count].iter() {
        if segment.copies_variable {
            expand_segment(segment, environment, &mut current, &mut length);
        } else {
            let bytes = segment.as_bytes();
            if length + bytes.len() > current.len() {
                break;
            }
            current[length..length + bytes.len()].copy_from_slice(bytes);
            length += bytes.len();
        }
        any_produced = true;
    }

    if !any_produced {
        return;
    }

    // O split só ocorre para texto sem aspas: um segmento entre aspas preserva
    // os espaços como parte do mesmo campo.
    let unquoted_may_split = word.segments[..word.segment_count]
        .iter()
        .any(|segment| segment.kind == SegmentKind::Unquoted);

    if unquoted_may_split {
        split_fields(&current[..length], output);
    } else if output.count < MAX_FIELDS {
        push_field(output, &current[..length]);
    }
}

fn expand_segment(
    segment: &Segment,
    environment: &Environment,
    output: &mut [u8; FIELD_SIZE],
    length: &mut usize,
) {
    let bytes = segment.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'$' {
            push_byte(output, length, bytes[index]);
            index += 1;
            continue;
        }
        let (name, next) = parse_reference(bytes, index + 1);
        if name.is_empty() {
            push_byte(output, length, b'$');
            index += 1;
            continue;
        }
        if let Some(value) = environment.get(name) {
            for &byte in value {
                push_byte(output, length, byte);
            }
        }
        index = next;
    }
}

/// Lê o nome da variável após `$`, aceitando `${NAME}`.
fn parse_reference(bytes: &[u8], start: usize) -> (&[u8], usize) {
    if bytes.get(start) == Some(&b'{') {
        let name_start = start + 1;
        if let Some(end) = bytes[name_start..].iter().position(|byte| *byte == b'}') {
            let end = name_start + end;
            return (&bytes[name_start..end], end + 1);
        }
        return (&[], start);
    }
    // Nomes curtos especiais que o shell expõe.
    if matches!(bytes.get(start), Some(b'?') | Some(b'$')) {
        return (&bytes[start..start + 1], start + 1);
    }
    let mut end = start;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    (&bytes[start..end], end)
}

fn push_byte(output: &mut [u8; FIELD_SIZE], length: &mut usize, byte: u8) {
    if *length < output.len() {
        output[*length] = byte;
        *length += 1;
    }
}

fn split_fields(input: &[u8], output: &mut ExpandedWord) {
    let mut index = 0usize;
    while index < input.len() {
        while index < input.len() && input[index].is_ascii_whitespace() {
            index += 1;
        }
        let start = index;
        while index < input.len() && !input[index].is_ascii_whitespace() {
            index += 1;
        }
        if start != index {
            push_field(output, &input[start..index]);
        }
    }
}

fn push_field(output: &mut ExpandedWord, field: &[u8]) {
    let _ = output.push_field(field);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::{tokenize, Token, Word, MAX_TOKENS};

    fn first_word(line: &str) -> (Token, usize) {
        let mut tokens = [Token::Word(Word::EMPTY); MAX_TOKENS];
        let count = tokenize(line.as_bytes(), &mut tokens).expect("tokenize");
        assert!(count > 0);
        (tokens[0], count)
    }

    fn expand_first(line: &str) -> ExpandedWord {
        let environment = Environment::new();
        let (token, _) = first_word(line);
        let Token::Word(word) = token else {
            panic!("expected word")
        };
        let mut output = ExpandedWord::new();
        expand(&word, &environment, &mut output);
        output
    }

    #[test]
    fn expands_simple_variable() {
        let output = expand_first("$PATH");
        assert_eq!(output.len(), 1);
        assert_eq!(output.as_slice(0), b"/bin");
    }

    #[test]
    fn expands_braced_variable_and_concatenation() {
        let output = expand_first("pre${HOME}post");
        assert_eq!(output.len(), 1);
        assert_eq!(output.as_slice(0), b"pre/homepost");
    }

    #[test]
    fn single_quotes_block_expansion() {
        let output = expand_first("'$HOME'");
        assert_eq!(output.len(), 1);
        assert_eq!(output.as_slice(0), b"$HOME");
    }

    #[test]
    fn double_quotes_keep_spaces_in_one_field() {
        let output = expand_first("\"a b\"");
        assert_eq!(output.len(), 1);
        assert_eq!(output.as_slice(0), b"a b");
    }

    #[test]
    fn unquoted_variable_splits_on_spaces() {
        let mut environment = Environment::new();
        environment.set(b"LIST", b"one two three", false);
        let (Token::Word(word), _) = first_word("$LIST") else {
            panic!("expected word")
        };
        let mut output = ExpandedWord::new();
        expand(&word, &environment, &mut output);
        assert_eq!(output.len(), 3);
        assert_eq!(output.as_slice(0), b"one");
        assert_eq!(output.as_slice(2), b"three");
    }
}
