//! Tokenizador do shell.
//!
//! Produz uma sequência de [`Word`]/operadores a partir de uma linha. O modelo
//! segue o POSIX o suficiente para um shell de uso geral:
//!
//! * *words* guardam os segmentos entre aspas para que a expansão de variáveis
//!   saiba quando não expandir (aspas simples) e quando preservar espaços
//!   (aspas duplas);
//! * operadores (`|`, `&&`, `||`, `;`, `<`, `>`, `>>`, `&`) são tokens próprios;
//! * `#` inicia comentário apenas quando começa uma word nova.

// Os tamanhos abaixo são deliberadamente modestos: as estruturas ficam na
// pilha do processo dentro do shell, que tem apenas `USER_STACK_SIZE` (32 KiB).
// Um comando com 8 palavras de até 48 bytes e 4 segmentos de aspas é mais que
// suficiente para uso interativo, e mantém a pilha sob controle.
pub const MAX_TOKENS: usize = 24;
/// Bytes por segmento. 64 cobre caminhos e argumentos usuais (incluindo URLs e
/// mensagens citadas); palavras maiores são recusadas com `WordTooLong` em vez
/// de silenciosamente cortadas.
pub const TOKEN_SIZE: usize = 64;
/// Segmentos por word: um literal e uma parte citada bastam para os casos
/// comuns (`prefix'quoted'`). Trechos mais fragmentados são combinados.
pub const MAX_SEGMENTS: usize = 2;
/// Aumentar `TOKEN_SIZE`/`MAX_SEGMENTS` também aumenta `Word`, `Command` e
/// `Entry`, que vivem na pilha de 128 KiB do processo. Se o shell voltar a
/// estourar a pilha, estes são os primeiros candidatos a reduzir.

/// Como um segmento deve ser tratado na expansão.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentKind {
    /// Texto sem aspas: sofre split implícito por espaço e expansão.
    Unquoted,
    /// `'...'`: totalmente literal, sem qualquer expansão.
    SingleQuoted,
    /// `"..."`: expande `$VAR` mas não faz split nem interpreta `*`.
    DoubleQuoted,
}

#[derive(Clone, Copy)]
pub struct Segment {
    pub bytes: [u8; TOKEN_SIZE],
    pub length: u8,
    pub kind: SegmentKind,
    /// Contém um `$` não escapado que precisa ser expandido em runtime.
    pub copies_variable: bool,
}

impl Segment {
    pub const EMPTY: Self = Self {
        bytes: [0; TOKEN_SIZE],
        length: 0,
        kind: SegmentKind::Unquoted,
        copies_variable: false,
    };

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.length)]
    }
}

/// Uma word é uma lista de segmentos que a expansão concatena.
#[derive(Clone, Copy)]
pub struct Word {
    pub segments: [Segment; MAX_SEGMENTS],
    pub segment_count: usize,
}

impl Word {
    pub const EMPTY: Self = Self {
        segments: [Segment::EMPTY; MAX_SEGMENTS],
        segment_count: 0,
    };

    /// Devolve a word como fatia contígua quando ela já é literal.
    ///
    /// Útil para reconhecer opções como `-n` ou nomes como `cd` sem alocar um
    /// buffer de expansão. `None` se houver variáveis ou mais de um segmento.
    pub fn literal_bytes(&self) -> Option<&[u8]> {
        if self.segment_count != 1 || self.segments[0].copies_variable {
            return None;
        }
        Some(self.segments[0].as_bytes())
    }
}

/// Gramática linear do shell: só palavras e operadores.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Pipe,
    And,
    Or,
    Semicolon,
    Background,
    RedirectIn,
    RedirectOut,
    RedirectAppend,
}

#[derive(Clone, Copy)]
pub enum Token {
    Word(Word),
    Operator(Operator),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LexError {
    UnterminatedQuote,
    TooManyTokens,
    WordTooLong,
}

/// Tokeniza `line` para `tokens`, devolvendo quantos tokens foram produzidos.
///
/// Este lexer é deliberadamente independente da linha: `parse` pode então
/// construir listas, pipelines e redirecionamentos sobre o vetor de tokens.
pub fn tokenize(line: &[u8], tokens: &mut [Token; MAX_TOKENS]) -> Result<usize, LexError> {
    let mut count = 0usize;
    let mut position = 0usize;

    while position < line.len() {
        let byte = line[position];
        // Espaço em branco separa words.
        if byte == b' ' || byte == b'\t' {
            position += 1;
            continue;
        }
        // Comentário: só quando começa uma word.
        if byte == b'#' {
            break;
        }

        if let Some((operator, width)) = operator_at(line, position) {
            if count == tokens.len() {
                return Err(LexError::TooManyTokens);
            }
            tokens[count] = Token::Operator(operator);
            count += 1;
            position += width;
            continue;
        }

        if count == tokens.len() {
            return Err(LexError::TooManyTokens);
        }
        let (word, next) = read_word(line, position)?;
        tokens[count] = Token::Word(word);
        count += 1;
        position = next;
    }

    Ok(count)
}

fn operator_at(line: &[u8], position: usize) -> Option<(Operator, usize)> {
    let rest = &line[position..];
    // Operadores mais longos primeiro, para `>>` não virar `>` + `>`.
    if rest.starts_with(b"&&") {
        return Some((Operator::And, 2));
    }
    if rest.starts_with(b"||") {
        return Some((Operator::Or, 2));
    }
    if rest.starts_with(b">>") {
        return Some((Operator::RedirectAppend, 2));
    }
    match rest[0] {
        b'|' => Some((Operator::Pipe, 1)),
        b';' => Some((Operator::Semicolon, 1)),
        b'&' => Some((Operator::Background, 1)),
        b'<' => Some((Operator::RedirectIn, 1)),
        b'>' => Some((Operator::RedirectOut, 1)),
        _ => None,
    }
}

/// Lê uma word a partir de `start`, acumulando segmentos conforme encontra
/// aspas. Devolve a word e a posição seguinte (após o separador).
fn read_word(line: &[u8], start: usize) -> Result<(Word, usize), LexError> {
    let mut word = Word::EMPTY;
    let mut position = start;

    while position < line.len() {
        let byte = line[position];
        if byte == b' ' || byte == b'\t' {
            break;
        }
        if operator_at(line, position).is_some() {
            break;
        }

        match byte {
            b'\'' => {
                position += 1;
                let begin = position;
                while position < line.len() && line[position] != b'\'' {
                    position += 1;
                }
                if position == line.len() {
                    return Err(LexError::UnterminatedQuote);
                }
                push_segment(
                    &mut word,
                    &line[begin..position],
                    SegmentKind::SingleQuoted,
                    false,
                )?;
                position += 1;
            }
            b'"' => {
                position += 1;
                let mut segment = Segment::EMPTY;
                segment.kind = SegmentKind::DoubleQuoted;
                while position < line.len() && line[position] != b'"' {
                    let byte = line[position];
                    // Dentro de aspas duplas só `\` escapa `"`, `\` e `$`.
                    if byte == b'\\' && position + 1 < line.len() {
                        let next = line[position + 1];
                        if matches!(next, b'"' | b'\\' | b'$' | b'`') {
                            push_byte(&mut segment, next)?;
                            position += 2;
                            continue;
                        }
                    }
                    if byte == b'$' {
                        segment.copies_variable = true;
                    }
                    push_byte(&mut segment, byte)?;
                    position += 1;
                }
                if position == line.len() {
                    return Err(LexError::UnterminatedQuote);
                }
                position += 1;
                push_segment_direct(&mut word, segment)?;
            }
            b'\\' => {
                position += 1;
                if position == line.len() {
                    return Err(LexError::UnterminatedQuote);
                }
                let escaped = [line[position]];
                push_segment(&mut word, &escaped, SegmentKind::Unquoted, false)?;
                position += 1;
            }
            _ => {
                let begin = position;
                let mut copies_variable = false;
                while position < line.len() {
                    let byte = line[position];
                    if byte == b' '
                        || byte == b'\t'
                        || byte == b'\''
                        || byte == b'"'
                        || byte == b'\\'
                        || operator_at(line, position).is_some()
                    {
                        break;
                    }
                    if byte == b'$' {
                        copies_variable = true;
                    }
                    position += 1;
                }
                push_segment(
                    &mut word,
                    &line[begin..position],
                    SegmentKind::Unquoted,
                    copies_variable,
                )?;
            }
        }
    }

    if word.segment_count == 0 {
        return Err(LexError::WordTooLong);
    }
    Ok((word, position))
}

fn push_segment(
    word: &mut Word,
    bytes: &[u8],
    kind: SegmentKind,
    copies_variable: bool,
) -> Result<(), LexError> {
    let mut segment = Segment::EMPTY;
    segment.kind = kind;
    segment.copies_variable = copies_variable;
    for &byte in bytes {
        push_byte(&mut segment, byte)?;
    }
    push_segment_direct(word, segment)
}

fn push_segment_direct(word: &mut Word, segment: Segment) -> Result<(), LexError> {
    if word.segment_count == MAX_SEGMENTS {
        return Err(LexError::WordTooLong);
    }
    word.segments[word.segment_count] = segment;
    word.segment_count += 1;
    Ok(())
}

fn push_byte(segment: &mut Segment, byte: u8) -> Result<(), LexError> {
    let length = usize::from(segment.length);
    if length == segment.bytes.len() {
        return Err(LexError::WordTooLong);
    }
    segment.bytes[length] = byte;
    segment.length += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(line: &str) -> (usize, [Token; MAX_TOKENS]) {
        let mut tokens = [Token::Word(Word::EMPTY); MAX_TOKENS];
        let count = tokenize(line.as_bytes(), &mut tokens).expect("tokenize");
        (count, tokens)
    }

    #[test]
    fn splits_words_and_operators() {
        // ls  -la  |  grep  txt  >  out   => 7 tokens
        let (count, tokens) = lex("ls -la | grep txt > out");
        assert_eq!(count, 7);
        assert!(matches!(tokens[0], Token::Word(_)));
        assert!(matches!(tokens[2], Token::Operator(Operator::Pipe)));
        assert!(matches!(tokens[5], Token::Operator(Operator::RedirectOut)));
    }

    #[test]
    fn recognises_double_character_operators() {
        let (count, tokens) = lex("a && b || c >> d");
        assert_eq!(count, 7);
        assert!(matches!(tokens[1], Token::Operator(Operator::And)));
        assert!(matches!(tokens[3], Token::Operator(Operator::Or)));
        assert!(matches!(
            tokens[5],
            Token::Operator(Operator::RedirectAppend)
        ));
    }

    #[test]
    fn quotes_are_tracked_per_segment() {
        let (_, tokens) = lex("echo a'b c' \"e f\"");
        // a'b c' -> 2 segmentos: literal "a" + citado "b c".
        let Token::Word(word) = tokens[1] else { panic!("word") };
        assert_eq!(word.segment_count, 2);
        assert_eq!(word.segments[0].kind, SegmentKind::Unquoted);
        assert_eq!(word.segments[1].kind, SegmentKind::SingleQuoted);
        // "e f" -> um único segmento com aspas duplas.
        let Token::Word(word) = tokens[2] else { panic!("word") };
        assert_eq!(word.segments[0].kind, SegmentKind::DoubleQuoted);
    }

    #[test]
    fn unquoted_dollar_marks_expansion() {
        let (_, tokens) = lex("echo $HOME '$HOME'");
        let Token::Word(word) = tokens[1] else {
            panic!("word")
        };
        assert!(word.segments[0].copies_variable);
        let Token::Word(word) = tokens[2] else {
            panic!("word")
        };
        assert!(!word.segments[0].copies_variable);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let (count, _) = lex("   # just a comment");
        assert_eq!(count, 0);
    }

    #[test]
    fn unterminated_quote_is_reported() {
        let mut tokens = [Token::Word(Word::EMPTY); MAX_TOKENS];
        assert_eq!(
            tokenize(b"echo \"open", &mut tokens),
            Err(LexError::UnterminatedQuote)
        );
    }
}
