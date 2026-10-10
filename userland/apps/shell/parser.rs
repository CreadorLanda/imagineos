//! Parser do shell.
//!
//! Reescreve o vetor de tokens do lexer em estruturas executáveis, uma *lista*
//! por vez. A gramática suportada:
//!
//! ```text
//! list      := pipeline ( (';' | '&&' | '||' | '&') pipeline )*
//! pipeline  := command ( '|' command )*
//! command   := word+ redirect*
//! redirect  := ('<' | '>' | '>>') word
//! ```
//!
//! Controle de fluxo (`if`/`for`/`while`) é reconhecido na primeira palavra de
//! um comando; o parser marca esses comandos como [`CommandKind::Keyword`] para
//! que o executor aplique a gramática composta, evitando misturar as duas
//! responsabilidades aqui.

use crate::lexer::{Operator, Token, Word};

/// Tamanho máximo de um pipeline (número de estágios).
pub const MAX_PIPELINE: usize = 3;
/// Palavras por comando (inclui o nome do programa).
pub const MAX_WORDS: usize = 6;
/// Entradas (pipelines) por linha: permite `a; b; c; d` e alguns `&&`/`||`.
pub const MAX_ENTRIES: usize = 6;

/// Um único comando: palavras e redirecionamentos.
#[derive(Clone, Copy)]
pub struct Command {
    pub words: [Word; MAX_WORDS],
    pub word_count: usize,
    pub stdin: Option<Word>,
    pub stdout: Option<Word>,
    pub append: bool,
}

impl Command {
    pub const EMPTY: Self = Self {
        words: [Word::EMPTY; MAX_WORDS],
        word_count: 0,
        stdin: None,
        stdout: None,
        append: false,
    };
}

/// Um pipeline é uma cadeia de comandos unidos por `|`.
#[derive(Clone, Copy)]
pub struct Pipeline {
    pub commands: [Command; MAX_PIPELINE],
    pub command_count: usize,
}

impl Pipeline {
    pub const EMPTY: Self = Self {
        commands: [Command::EMPTY; MAX_PIPELINE],
        command_count: 0,
    };
}

/// Operador que liga este pipeline ao próximo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connector {
    /// `;` — executa o seguinte de qualquer forma.
    Always,
    /// `&&` — só se o anterior teve sucesso.
    OnSuccess,
    /// `||` — só se o anterior falhou.
    OnFailure,
    /// `&` — não espera o término.
    Background,
    /// Fim da entrada.
    End,
}

#[derive(Clone, Copy)]
pub struct Entry {
    pub pipeline: Pipeline,
    /// Operador lido *após* este pipeline, que decide o próximo.
    pub connector: Connector,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Um operador apareceu onde se esperava um comando.
    UnexpectedOperator,
    /// O pipeline excede [`MAX_PIPELINE`] estágios.
    PipelineTooLong,
    /// Um comando excede [`MAX_WORDS`] palavras.
    TooManyWords,
    /// Redirecionamento sem destino.
    MissingRedirectionTarget,
}

/// Lê uma *lista* completa de `tokens` para `entries`.
///
/// Devolve o número de entradas preenchidas. Cada entrada carrega o conector
/// que a liga à seguinte, o que basta para o executor decidir se pula o resto
/// (curto-circuito de `&&`/`||`).
pub fn parse_list(
    tokens: &[Token],
    entries: &mut [Entry; MAX_ENTRIES],
) -> Result<usize, ParseError> {
    let mut index = 0usize;
    let mut entry_count = 0usize;

    while index < tokens.len() {
        if entry_count == entries.len() {
            return Err(ParseError::PipelineTooLong);
        }
        let (pipeline, next, connector) = parse_pipeline(tokens, index)?;
        entries[entry_count] = Entry { pipeline, connector };
        entry_count += 1;
        index = next;
        if connector == Connector::End {
            break;
        }
    }

    Ok(entry_count)
}

fn parse_pipeline(tokens: &[Token], start: usize) -> Result<(Pipeline, usize, Connector), ParseError> {
    let mut pipeline = Pipeline::EMPTY;
    let mut index = start;

    loop {
        if index >= tokens.len() {
            return Err(ParseError::UnexpectedOperator);
        }
        if pipeline.command_count == MAX_PIPELINE {
            return Err(ParseError::PipelineTooLong);
        }
        let (command, next) = parse_command(tokens, index)?;
        pipeline.commands[pipeline.command_count] = command;
        pipeline.command_count += 1;
        index = next;

        // Se o próximo token for `|`, o pipeline continua.
        if let Some(Token::Operator(Operator::Pipe)) = tokens.get(index) {
            index += 1;
            continue;
        }
        break;
    }

    let connector = match tokens.get(index) {
        None => Connector::End,
        Some(Token::Operator(Operator::Semicolon)) => Connector::Always,
        Some(Token::Operator(Operator::And)) => Connector::OnSuccess,
        Some(Token::Operator(Operator::Or)) => Connector::OnFailure,
        Some(Token::Operator(Operator::Background)) => Connector::Background,
        Some(_) => return Err(ParseError::UnexpectedOperator),
    };
    if connector != Connector::End {
        index += 1;
    }
    Ok((pipeline, index, connector))
}

fn parse_command(tokens: &[Token], start: usize) -> Result<(Command, usize), ParseError> {
    let mut command = Command::EMPTY;
    let mut index = start;
    // Slot que espera a próxima palavra, quando ela for alvo de um
    // redirecionamento. Guardado como variável local: nada é compartilhado.
    let mut pending = RedirectSlot::None;

    while index < tokens.len() {
        match tokens[index] {
            Token::Word(word) => {
                match pending {
                    RedirectSlot::In => command.stdin = Some(word),
                    RedirectSlot::Out => command.stdout = Some(word),
                    RedirectSlot::None => {
                        if command.word_count == MAX_WORDS {
                            return Err(ParseError::TooManyWords);
                        }
                        command.words[command.word_count] = word;
                        command.word_count += 1;
                    }
                }
                pending = RedirectSlot::None;
                index += 1;
            }
            Token::Operator(operator @ (Operator::RedirectIn | Operator::RedirectOut | Operator::RedirectAppend)) => {
                if pending != RedirectSlot::None {
                    return Err(ParseError::MissingRedirectionTarget);
                }
                if operator == Operator::RedirectIn {
                    command.stdin = None;
                    pending = RedirectSlot::In;
                } else {
                    command.stdout = None;
                    command.append = operator == Operator::RedirectAppend;
                    pending = RedirectSlot::Out;
                }
                index += 1;
            }
            Token::Operator(_) => break,
        }
    }

    if pending != RedirectSlot::None {
        return Err(ParseError::MissingRedirectionTarget);
    }
    if command.word_count == 0 {
        return Err(ParseError::UnexpectedOperator);
    }
    Ok((command, index))
}

/// Slot aguardando a palavra que é o destino de um redirecionamento.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RedirectSlot {
    None,
    In,
    Out,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::{tokenize, Token, Word};

    fn parse(line: &str) -> (usize, [Entry; MAX_ENTRIES]) {
        let mut tokens = [Token::Word(Word::EMPTY); crate::lexer::MAX_TOKENS];
        let count = tokenize(line.as_bytes(), &mut tokens).expect("tokenize");
        let mut entries = [Entry {
            pipeline: Pipeline::EMPTY,
            connector: Connector::End,
        }; MAX_ENTRIES];
        let entry_count = parse_list(&tokens[..count], &mut entries).expect("parse");
        (entry_count, entries)
    }

    fn literal(word: &Word) -> [u8; crate::lexer::TOKEN_SIZE] {
        let mut buffer = [0u8; crate::lexer::TOKEN_SIZE];
        let bytes = word.literal_bytes().expect("literal word");
        buffer[..bytes.len()].copy_from_slice(bytes);
        buffer
    }

    #[test]
    fn parses_simple_command_with_words() {
        let (count, entries) = parse("ls -la");
        assert_eq!(count, 1);
        assert_eq!(entries[0].pipeline.command_count, 1);
        let command = &entries[0].pipeline.commands[0];
        assert_eq!(command.word_count, 2);
        assert_eq!(&literal(&command.words[0])[..2], b"ls");
        assert_eq!(&literal(&command.words[1])[..3], b"-la");
    }

    #[test]
    fn parses_pipeline_stages() {
        let (count, entries) = parse("cat file | grep x | wc");
        assert_eq!(count, 1);
        assert_eq!(entries[0].pipeline.command_count, 3);
    }

    #[test]
    fn records_connectors_between_entries() {
        let (count, entries) = parse("a && b || c ; d");
        assert_eq!(count, 4);
        assert_eq!(entries[0].connector, Connector::OnSuccess);
        assert_eq!(entries[1].connector, Connector::OnFailure);
        assert_eq!(entries[2].connector, Connector::Always);
        assert_eq!(entries[3].connector, Connector::End);
    }

    #[test]
    fn parses_redirections() {
        let (_, entries) = parse("echo hi > out.txt");
        let command = &entries[0].pipeline.commands[0];
        assert!(command.stdout.is_some());
        assert!(!command.append);
        assert_eq!(command.word_count, 2);

        let (_, entries) = parse("cat < in.txt");
        let command = &entries[0].pipeline.commands[0];
        assert!(command.stdin.is_some());
        assert_eq!(command.word_count, 1);
    }

    #[test]
    fn append_redirection_is_flagged() {
        let (_, entries) = parse("echo hi >> log");
        let command = &entries[0].pipeline.commands[0];
        assert!(command.append);
    }

    #[test]
    fn rejects_dangling_redirection() {
        let mut tokens = [Token::Word(Word::EMPTY); crate::lexer::MAX_TOKENS];
        let count = tokenize(b"echo hi >", &mut tokens).expect("tokenize");
        let mut entries = [Entry {
            pipeline: Pipeline::EMPTY,
            connector: Connector::End,
        }; MAX_ENTRIES];
        assert_eq!(
            parse_list(&tokens[..count], &mut entries),
            Err(ParseError::MissingRedirectionTarget)
        );
    }

    #[test]
    fn treats_background_as_a_connector() {
        let (count, entries) = parse("sleep 10 & echo done");
        assert_eq!(count, 2);
        assert_eq!(entries[0].connector, Connector::Background);
    }
}
