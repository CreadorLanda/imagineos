//! Executor do shell: liga o parser às primitivas do kernel.
//!
//! A lista de [`Entry`] do parser é executada em ordem, respeitando
//! `;`/`&&`/`||` (com curto-circuito). Para um pipeline, cria um pipe entre
//! cada par de estágios, lança cada um com `process::spawn` e passa as pontas
//! via [`SpawnIo`]; o shell fecha imediatamente os descritores que entregou,
//! senão o leitor nunca veria EOF.
//!
//! Não há `waitpid`: como o escalonador é cooperativo, o shell descobre o fim
//! de um estágio por polling de `process::list`, como o `init` e o `getty` já
//! fazem.

use imagineos::abi::SpawnIo;
use imagineos::fs;
use imagineos::process;

use crate::env::{self, Environment, EnvironmentPairs};
use crate::expand::{expand, ExpandedWord, FIELD_SIZE};
use crate::lexer::Word;
use crate::parser::{Command, Connector, Entry, MAX_PIPELINE, MAX_WORDS};
use crate::uri;

/// Código de saída de um comando.
pub type Status = i32;

pub const STATUS_SUCCESS: Status = 0;
pub const STATUS_FAILURE: Status = 1;

pub struct Executor<'a> {
    pub environment: &'a mut Environment,
    pub cwd: &'a mut [u8; uri::MAX_PATH],
    pub cwd_length: &'a mut usize,
    /// Ligado por `exit`/`logout`: o laço principal encerra após a linha.
    pub should_exit: bool,
    pub exit_status: Status,
    /// Buffers de `NAME=VALUE` reaproveitados a cada `spawn`.
    pairs: EnvironmentPairs,
}

impl<'a> Executor<'a> {
    pub fn new(environment: &'a mut Environment, cwd: &'a mut [u8; uri::MAX_PATH], cwd_length: &'a mut usize) -> Self {
        Self {
            environment,
            cwd,
            cwd_length,
            should_exit: false,
            exit_status: 0,
            pairs: EnvironmentPairs::new(),
        }
    }
    /// Executa a lista completa, devolvendo o status do último comando.
    pub fn run_list(&mut self, entries: &[Entry]) -> Status {
        let mut status = STATUS_SUCCESS;
        let mut index = 0usize;

        while index < entries.len() {
            let entry = &entries[index];
            status = self.run_pipeline(&entry.pipeline);
            self.environment.set_status(i64::from(status));
            if self.should_exit {
                return status;
            }

            match entry.connector {
                Connector::End => break,
                Connector::Always | Connector::Background => index += 1,
                Connector::OnSuccess => {
                    index += 1;
                    if status != STATUS_SUCCESS {
                        index = skip_alternates(entries, index);
                    }
                }
                Connector::OnFailure => {
                    index += 1;
                    if status == STATUS_SUCCESS {
                        index = skip_alternates(entries, index);
                    }
                }
            }
        }
        status
    }

    /// Executa um pipeline, ligando os estágios com pipes quando houver mais de
    /// um comando.
    fn run_pipeline(&mut self, pipeline: &crate::parser::Pipeline) -> Status {
        match pipeline.command_count {
            0 => STATUS_FAILURE,
            1 => self.run_single(&pipeline.commands[0]),
            2..=MAX_PIPELINE => {
                self.run_pipeline_processes(&pipeline.commands[..pipeline.command_count])
            }
            _ => {
                write_error(b"shell: pipeline longo demais\n");
                STATUS_FAILURE
            }
        }
    }

    // ------------------------------------------------------------------
    // Comando único
    // ------------------------------------------------------------------

    fn run_single(&mut self, command: &Command) -> Status {
        let has_redirection = command.stdin.is_some() || command.stdout.is_some();

        // Builtin com redirecionamento: aplicamos `dup2` no próprio shell,
        // executamos o builtin e restauramos os descritores originais. É o que
        // um shell POSIX faz quando o builtin roda sem processo separado.
        if has_redirection && self.is_builtin(command) {
            return self.run_builtin_redirected(command);
        }
        if !has_redirection {
            if let Some(status) = self.run_builtin(command) {
                return status;
            }
        }
        self.run_external(command)
    }

    /// Verdadeiro se a primeira palavra do comando é um builtin.
    fn is_builtin(&self, command: &Command) -> bool {
        match self.first_word(command) {
            Some((name, length)) => is_builtin_name(&name[..length]),
            None => false,
        }
    }

    /// Executa um builtin com `stdin`/`stdout` redirecionados.
    ///
    /// Guarda os descritores 0/1 atuais, instala os arquivos pedidos, roda o
    /// builtin e restaura — sempre restaurando, mesmo em erro.
    fn run_builtin_redirected(&mut self, command: &Command) -> Status {
        let mut saved_stdin = usize::MAX;
        let mut saved_stdout = usize::MAX;

        if let Some(target) = &command.stdin {
            let file = match self.open_redirection(target, false, false) {
                Ok(fd) => fd,
                Err(message) => {
                    write_error(message);
                    return STATUS_FAILURE;
                }
            };
            saved_stdin = match process::duplicate(fs::stdin()) {
                Ok(fd) => fd,
                Err(_) => {
                    let _ = fs::close(file);
                    write_error(b"shell: nao foi possivel salvar stdin\n");
                    return STATUS_FAILURE;
                }
            };
            if process::duplicate_to(file, fs::stdin()).is_err() {
                let _ = fs::close(file);
                let _ = fs::close(saved_stdin);
                write_error(b"shell: nao foi possivel redirecionar stdin\n");
                return STATUS_FAILURE;
            }
        }

        if let Some(target) = &command.stdout {
            let file = match self.open_redirection(target, true, command.append) {
                Ok(fd) => fd,
                Err(message) => {
                    self.restore_stdin(saved_stdin);
                    write_error(message);
                    return STATUS_FAILURE;
                }
            };
            saved_stdout = match process::duplicate(fs::stdout()) {
                Ok(fd) => fd,
                Err(_) => {
                    let _ = fs::close(file);
                    self.restore_stdin(saved_stdin);
                    write_error(b"shell: nao foi possivel salvar stdout\n");
                    return STATUS_FAILURE;
                }
            };
            if process::duplicate_to(file, fs::stdout()).is_err() {
                let _ = fs::close(file);
                let _ = fs::close(saved_stdout);
                self.restore_stdin(saved_stdin);
                write_error(b"shell: nao foi possivel redirecionar stdout\n");
                return STATUS_FAILURE;
            }
        }

        let status = self.run_builtin(command).unwrap_or(STATUS_FAILURE);

        // Restaura os descritores originais e fecha as cópias temporárias.
        if saved_stdout != usize::MAX {
            let _ = process::duplicate_to(saved_stdout, fs::stdout());
            let _ = fs::close(saved_stdout);
        }
        self.restore_stdin(saved_stdin);
        status
    }

    fn restore_stdin(&self, saved: usize) {
        if saved != usize::MAX {
            let _ = process::duplicate_to(saved, fs::stdin());
            let _ = fs::close(saved);
        }
    }

    /// Tenta executar `command` como builtin. `None` se não for builtin.
    fn run_builtin(&mut self, command: &Command) -> Option<Status> {
        // `first_word` devolve também o comprimento: comparar o array inteiro
        // (com os zeros de preenchimento) contra `b"echo"` nunca casaria.
        let (name, name_length) = self.first_word(command)?;
        let name = &name[..name_length];
        let arguments = &command.words[1..command.word_count];
        match name {
            b"cd" => Some(self.builtin_cd(arguments)),
            b"pwd" => Some(self.builtin_pwd()),
            b"echo" => Some(self.builtin_echo(arguments)),
            b"export" => Some(self.builtin_export(arguments)),
            b"unset" => Some(self.builtin_unset(arguments)),
            b"set" => Some(self.print_environment()),
            b"exit" | b"logout" => {
                self.should_exit = true;
                self.exit_status = 0;
                Some(0)
            }
            b":" | b"true" => Some(0),
            b"false" => Some(1),
            b"help" => Some(self.builtin_help()),
            _ => None,
        }
    }

    fn builtin_cd(&mut self, arguments: &[Word]) -> Status {
        if arguments.len() > 1 {
            write_error(b"cd: uso: cd [diretorio]\n");
            return STATUS_FAILURE;
        }
        let mut target = ExpandedWord::new();
        if arguments.is_empty() {
            let home = self.environment.get(b"HOME").unwrap_or(b"/");
            let mut buffer = [0u8; uri::MAX_PATH];
            let length = home.len().min(buffer.len());
            buffer[..length].copy_from_slice(&home[..length]);
            let _ = target_push(&mut target, &buffer[..length]);
        } else {
            expand(&arguments[0], self.environment, &mut target);
        }
        if target.is_empty() {
            write_error(b"cd: caminho vazio\n");
            return STATUS_FAILURE;
        }

        let home = self.environment.get(b"HOME").unwrap_or(b"/");
        let mut tilde = [0u8; uri::MAX_PATH];
        let Some(tilde_length) = uri::expand_tilde(target.as_slice(0), home, &mut tilde) else {
            write_error(b"cd: caminho invalido\n");
            return STATUS_FAILURE;
        };
        let mut resolved = [0u8; uri::MAX_PATH];
        let Some(length) = uri::resolve(
            &self.cwd[..*self.cwd_length],
            &tilde[..tilde_length],
            &mut resolved,
        ) else {
            write_error(b"cd: caminho longo demais\n");
            return STATUS_FAILURE;
        };
        let Ok(path) = core::str::from_utf8(&resolved[..length]) else {
            write_error(b"cd: codificacao invalida\n");
            return STATUS_FAILURE;
        };
        match fs::is_dir(path) {
            Ok(true) => {}
            Ok(false) => {
                write_error(b"cd: diretorio nao encontrado\n");
                return STATUS_FAILURE;
            }
            Err(_) => {
                write_error(b"cd: nao foi possivel inspecionar o diretorio\n");
                return STATUS_FAILURE;
            }
        }
        self.cwd[..length].copy_from_slice(&resolved[..length]);
        *self.cwd_length = length;
        let _ = self.environment.set(b"PWD", &resolved[..length], true);
        STATUS_SUCCESS
    }

    fn builtin_pwd(&mut self) -> Status {
        write(&self.cwd[..*self.cwd_length]);
        write(b"\n");
        STATUS_SUCCESS
    }

    fn builtin_echo(&mut self, arguments: &[Word]) -> Status {
        let mut start = 0usize;
        let mut newline = true;
        if let Some(first) = arguments.first() {
            if first.literal_bytes().is_some_and(|bytes| bytes == b"-n") {
                newline = false;
                start = 1;
            }
        }
        for (index, word) in arguments[start..].iter().enumerate() {
            if index > 0 {
                write(b" ");
            }
            let mut expanded = ExpandedWord::new();
            expand(word, self.environment, &mut expanded);
            for field in 0..expanded.len() {
                if field > 0 {
                    write(b" ");
                }
                write(expanded.as_slice(field));
            }
        }
        if newline {
            write(b"\n");
        }
        STATUS_SUCCESS
    }

    fn builtin_export(&mut self, arguments: &[Word]) -> Status {
        if arguments.is_empty() {
            return self.print_environment();
        }
        for word in arguments {
            let mut expanded = ExpandedWord::new();
            expand(word, self.environment, &mut expanded);
            for field in 0..expanded.len() {
                // Copia o campo: `set` toma `&mut self.environment`, então não
                // podemos manter um empréstimo de `expanded` derivado dele.
                let mut storage = [0u8; FIELD_SIZE];
                let bytes = expanded.as_slice(field);
                if bytes.len() > storage.len() {
                    continue;
                }
                storage[..bytes.len()].copy_from_slice(bytes);
                let bytes = &storage[..bytes.len()];
                if let Some(equal) = bytes.iter().position(|byte| *byte == b'=') {
                    if !self.environment.set(&bytes[..equal], &bytes[equal + 1..], true) {
                        write_error(b"export: nome invalido ou valor longo demais\n");
                    }
                } else {
                    let mut value_buffer = [0u8; env::VALUE_SIZE];
                    let value_length = self
                        .environment
                        .get(bytes)
                        .map(|value| value.len().min(value_buffer.len()))
                        .unwrap_or(0);
                    if value_length != 0 {
                        if let Some(value) = self.environment.get(bytes) {
                            value_buffer[..value_length].copy_from_slice(&value[..value_length]);
                        }
                    }
                    let _ = self.environment.set(bytes, &value_buffer[..value_length], true);
                }
            }
        }
        STATUS_SUCCESS
    }

    fn builtin_unset(&mut self, arguments: &[Word]) -> Status {
        for word in arguments {
            if let Some(bytes) = word.literal_bytes() {
                self.environment.unset(bytes);
            }
        }
        STATUS_SUCCESS
    }

    fn builtin_help(&mut self) -> Status {
        write(
            b"builtins: cd pwd echo export unset set exit true false help\n\
              operadores: | && || ; > >> < &   variaveis: $VAR ${VAR} $? $$ ' ' \" \"\n",
        );
        STATUS_SUCCESS
    }

    fn print_environment(&mut self) -> Status {
        for variable in self.environment.iter_exported() {
            write(&variable.name[..variable.name_length]);
            write(b"=");
            write(&variable.value[..variable.value_length]);
            write(b"\n");
        }
        STATUS_SUCCESS
    }

    // ------------------------------------------------------------------
    // Comando externo
    // ------------------------------------------------------------------

    fn run_external(&mut self, command: &Command) -> Status {
        let mut io = SpawnIo::default();
        let mut opened = [usize::MAX; 2];

        if let Some(target) = &command.stdin {
            match self.open_redirection(target, false, false) {
                Ok(fd) => {
                    opened[0] = fd;
                    io.stdin_fd = fd as i64;
                }
                Err(message) => {
                    write_error(message);
                    return STATUS_FAILURE;
                }
            }
        }
        if let Some(target) = &command.stdout {
            match self.open_redirection(target, true, command.append) {
                Ok(fd) => {
                    opened[1] = fd;
                    io.stdout_fd = fd as i64;
                }
                Err(message) => {
                    if opened[0] != usize::MAX {
                        let _ = fs::close(opened[0]);
                    }
                    write_error(message);
                    return STATUS_FAILURE;
                }
            }
        }

        // `fork` + `execve` + `waitpid`, o caminho POSIX. Ao contrário do
        // `spawn`, isto permite ler o código de saída do programa e devolvê-lo
        // como status do shell (`$?`).
        match process::fork() {
            Ok(0) => {
                // Filho: instala as redireções em stdin/stdout e troca a imagem.
                if io.stdin_fd != SpawnIo::INHERIT {
                    if process::duplicate_to(io.stdin_fd as usize, fs::stdin()).is_err() {
                        process::exit(1);
                    }
                }
                if io.stdout_fd != SpawnIo::INHERIT {
                    if process::duplicate_to(io.stdout_fd as usize, fs::stdout()).is_err() {
                        process::exit(1);
                    }
                }
                // Fecha as cópias temporárias que o shell abriu; o filho já
                // tem o que precisa em 0/1.
                for fd in opened {
                    if fd != usize::MAX {
                        let _ = fs::close(fd);
                    }
                }
                self.execve_stage(command)
            }
            Ok(pid) => {
                // Pai: fecha as cópias que só o filho usava e espera-o.
                for fd in opened {
                    if fd != usize::MAX {
                        let _ = fs::close(fd);
                    }
                }
                match process::waitpid(pid) {
                    Ok((_, raw)) => process::exit_code(raw),
                    Err(_) => STATUS_FAILURE,
                }
            }
            Err(_) => {
                for fd in opened {
                    if fd != usize::MAX {
                        let _ = fs::close(fd);
                    }
                }
                write_error(b"shell: falha ao criar processo\n");
                STATUS_FAILURE
            }
        }
    }

    fn open_redirection(
        &mut self,
        target: &Word,
        write: bool,
        append: bool,
    ) -> Result<usize, &'static [u8]> {
        let mut expanded = ExpandedWord::new();
        expand(target, self.environment, &mut expanded);
        if expanded.is_empty() {
            return Err(b"shell: redirecionamento sem destino\n");
        }
        let mut target_bytes = [0u8; FIELD_SIZE];
        let bytes = expanded.as_slice(0);
        if bytes.len() > target_bytes.len() {
            return Err(b"shell: caminho de redirecionamento longo demais\n");
        }
        target_bytes[..bytes.len()].copy_from_slice(bytes);
        let bytes = &target_bytes[..bytes.len()];
        let mut resolved = [0u8; uri::MAX_PATH];
        let Some(length) = uri::resolve(&self.cwd[..*self.cwd_length], bytes, &mut resolved) else {
            return Err(b"shell: caminho de redirecionamento longo demais\n");
        };
        let Ok(path) = core::str::from_utf8(&resolved[..length]) else {
            return Err(b"shell: caminho de redirecionamento invalido\n");
        };
        let options = if write {
            if append {
                fs::OpenOptions::new().write(true).create(true).append(true)
            } else {
                fs::OpenOptions::new().write(true).create(true).truncate(true)
            }
        } else {
            fs::OpenOptions::new().read(true)
        };
        fs::open(path, options)
            .map(|file| file.descriptor())
            .map_err(|_| b"shell: nao foi possivel abrir o arquivo\n" as &[u8])
    }

    // ------------------------------------------------------------------
    // Pipelines
    // ------------------------------------------------------------------

    fn run_pipeline_processes(&mut self, commands: &[Command]) -> Status {
        let stage_count = commands.len();

        // Passo 1: criar todos os pipes com antecedência, como faz um shell
        // POSIX antes de qualquer `fork`.
        let mut pipes = [(0usize, 0usize); MAX_PIPELINE - 1];
        let mut created = 0usize;
        for _ in 0..stage_count - 1 {
            match process::pipe() {
                Ok(ends) => {
                    pipes[created] = ends;
                    created += 1;
                }
                Err(_) => {
                    close_pipes(&pipes[..created]);
                    write_error(b"shell: nao foi possivel criar um pipe\n");
                    return STATUS_FAILURE;
                }
            }
        }

        // Passo 2: um `fork` por estágio. No filho, instalamos as pontas do
        // pipe com `dup2` e trocamos a imagem com `execve`; o pai fecha as
        // pontas que não usa.
        let mut pids = [0usize; MAX_PIPELINE];
        let mut launched = 0usize;
        for (index, command) in commands.iter().enumerate() {
            match process::fork() {
                Ok(0) => {
                    // --- Filho: os descritores desta imagem são todas as pontas
                    // de pipe conhecidas. Fecha as que não lhe pertencem e
                    // instala stdin/stdout com `dup2`.
                    self.child_setup_pipes(&pipes[..created], index, stage_count);
                    // `execve_stage` nunca retorna: ou troca a imagem ou chama
                    // `exit` com um código de erro.
                    self.execve_stage(command)
                }
                Ok(pid) => {
                    pids[launched] = pid as usize;
                    launched += 1;
                }
                Err(_) => {
                    close_pipes(&pipes[..created]);
                    for &pid in &pids[..launched] {
                        let _ = process::waitpid(pid as i64);
                    }
                    write_error(b"shell: falha ao criar processo\n");
                    return STATUS_FAILURE;
                }
            }
        }

        // Passo 3: o pai não usa nenhuma ponta; fechá-las faz o EOF chegar.
        close_pipes(&pipes[..created]);

        // Passo 4: coletar todos os filhos e guardar o status do **último**,
        // como o POSIX define para pipelines.
        let mut last_status = STATUS_SUCCESS;
        for &pid in &pids[..launched] {
            if let Ok((_, raw)) = process::waitpid(pid as i64) {
                last_status = process::exit_code(raw);
            }
        }
        last_status
    }

    /// No filho: fecha as pontas de pipe alheias e instala as próprias em
    /// stdin/stdout. Chamado logo após `fork`, antes de `execve`.
    fn child_setup_pipes(&self, pipes: &[(usize, usize)], index: usize, stage_count: usize) {
        // Fecha tudo, exceto as duas pontas que este estágio usa. Manter uma
        // ponta aberta impediria o EOF de chegar — exatamente o bug que travava
        // `ls | cat`.
        let used_stdin = if index == 0 { usize::MAX } else { pipes[index - 1].0 };
        let used_stdout = if index + 1 == stage_count {
            usize::MAX
        } else {
            pipes[index].1
        };
        for &(read_end, write_end) in pipes {
            for fd in [read_end, write_end] {
                if fd != used_stdin && fd != used_stdout {
                    let _ = fs::close(fd);
                }
            }
        }
        if used_stdin != usize::MAX {
            let _ = process::duplicate_to(used_stdin, fs::stdin());
            let _ = fs::close(used_stdin);
        }
        if used_stdout != usize::MAX {
            let _ = process::duplicate_to(used_stdout, fs::stdout());
            let _ = fs::close(used_stdout);
        }
    }

    /// No filho: resolve o programa do estágio e substitui a imagem.
    ///
    /// Builtins (como `echo`) não têm binário próprio, então são executados
    /// pelo próprio shell via `/bin/shell -c "<comando>"`.
    fn execve_stage(&mut self, command: &Command) -> ! {
        let mut storage = [[0u8; FIELD_SIZE]; MAX_WORDS];
        let mut lengths = [0usize; MAX_WORDS];
        let mut count = 0usize;
        for word in command.words[..command.word_count].iter() {
            let mut expanded = ExpandedWord::new();
            expand(word, self.environment, &mut expanded);
            for field in 0..expanded.len() {
                if count == MAX_WORDS {
                    break;
                }
                let bytes = expanded.as_slice(field);
                if bytes.len() > FIELD_SIZE {
                    break;
                }
                storage[count][..bytes.len()].copy_from_slice(bytes);
                lengths[count] = bytes.len();
                count += 1;
            }
        }
        if count == 0 {
            write_error(b"shell: comando vazio\n");
            process::exit(2);
        }

        self.environment.fill_pairs(&mut self.pairs);
        let environment = self.pairs.as_strs();
        let environment = &environment[..self.pairs.len()];

        if is_builtin_name(&storage[0][..lengths[0]]) {
            let mut line = [0u8; FIELD_SIZE];
            let length = join_arguments(&storage, &lengths, count, &mut line);
            let Ok(line) = core::str::from_utf8(&line[..length]) else {
                process::exit(2);
            };
            if process::execve("/bin/shell", &["/bin/shell", "-c", line], environment).is_err() {
                write_error(b"shell: nao foi possivel executar o builtin\n");
                process::exit(126);
            }
            process::exit(126);
        }

        let mut argv = [""; MAX_WORDS];
        for (index, item) in argv.iter_mut().enumerate().take(count) {
            match core::str::from_utf8(&storage[index][..lengths[index]]) {
                Ok(text) => *item = text,
                Err(_) => process::exit(2),
            }
        }

        let mut path_buffer = [0u8; uri::MAX_PATH];
        let Some(path_length) = self.resolve_command(&storage[0][..lengths[0]], &mut path_buffer)
        else {
            write_error(b"shell: comando nao encontrado\n");
            process::exit(127);
        };
        let Ok(path) = core::str::from_utf8(&path_buffer[..path_length]) else {
            process::exit(2);
        };
        if process::execve(path, &argv[..count], environment).is_err() {
            write_error(b"shell: falha ao executar o comando\n");
            process::exit(126);
        }
        process::exit(126);
    }

    /// Expande e lança um comando como novo processo.
    /// Resolve um nome em caminho absoluto, considerando `PATH`.
    fn resolve_command(&self, name: &[u8], output: &mut [u8; uri::MAX_PATH]) -> Option<usize> {
        // Caminho explícito: resolve em relação ao cwd.
        if name.contains(&b'/') {
            let mut resolved = [0u8; uri::MAX_PATH];
            let length = uri::resolve(&self.cwd[..*self.cwd_length], name, &mut resolved)?;
            let path = core::str::from_utf8(&resolved[..length]).ok()?;
            if fs::is_file(path).unwrap_or(false) {
                output[..length].copy_from_slice(&resolved[..length]);
                return Some(length);
            }
            return None;
        }

        // Copia o PATH: o laço abaixo chama `fs::is_file`, e manter o empréstimo
        // de `self.environment` tornaria `self` imutavelmente emprestado por
        // todo o laço — o que é aceitável aqui, mas confuso. Uma cópia local
        // mantém a função simples e previsível.
        let mut path_storage = [0u8; env::VALUE_SIZE];
        let path_length = self
            .environment
            .get(b"PATH")
            .map(|value| value.len().min(path_storage.len()))
            .unwrap_or(0);
        if path_length != 0 {
            if let Some(value) = self.environment.get(b"PATH") {
                path_storage[..path_length].copy_from_slice(&value[..path_length]);
            }
        }
        let path_variable = &path_storage[..path_length];
        for directory in path_variable.split(|byte| *byte == b':') {
            let directory = if directory.is_empty() {
                &self.cwd[..*self.cwd_length]
            } else {
                directory
            };
            let mut candidate = [0u8; uri::MAX_PATH];
            let mut length = directory.len();
            if length >= candidate.len() {
                continue;
            }
            candidate[..length].copy_from_slice(directory);
            if length == 0 || candidate[length - 1] != b'/' {
                candidate[length] = b'/';
                length += 1;
            }
            if length + name.len() > candidate.len() {
                continue;
            }
            candidate[length..length + name.len()].copy_from_slice(name);
            let full = length + name.len();
            let Ok(text) = core::str::from_utf8(&candidate[..full]) else {
                continue;
            };
            if fs::is_file(text).unwrap_or(false) {
                output[..full].copy_from_slice(&candidate[..full]);
                return Some(full);
            }
        }
        None
    }

    // ------------------------------------------------------------------
    // Utilidades
    // ------------------------------------------------------------------

    /// Expande a primeira palavra (nome do comando) e devolve o buffer e o
    /// seu comprimento real.
    fn first_word(&self, command: &Command) -> Option<([u8; FIELD_SIZE], usize)> {
        let word = command.words.first()?;
        let mut expanded = ExpandedWord::new();
        expand(word, self.environment, &mut expanded);
        if expanded.is_empty() {
            return None;
        }
        let bytes = expanded.as_slice(0);
        let mut buffer = [0u8; FIELD_SIZE];
        if bytes.len() > buffer.len() {
            return None;
        }
        buffer[..bytes.len()].copy_from_slice(bytes);
        Some((buffer, bytes.len()))
    }

}

/// Escreve no console. É uma função livre para que os builtins possam escrever
/// dados emprestados de `self` sem conflitar com um empréstimo de `&self`.
fn write(bytes: &[u8]) {
    let _ = imagineos::console::write_all(bytes);
}

fn write_error(message: &[u8]) {
    let _ = imagineos::console::write_all(message);
}

/// Reconstrói a linha de comando a partir de `argv` já expandido, para passar
/// a `/bin/shell -c`. Os argumentos são separados por espaço.
fn join_arguments(
    storage: &[[u8; FIELD_SIZE]; MAX_WORDS],
    lengths: &[usize; MAX_WORDS],
    count: usize,
    output: &mut [u8; FIELD_SIZE],
) -> usize {
    let mut length = 0usize;
    for index in 0..count {
        if index != 0 {
            if length == output.len() {
                break;
            }
            output[length] = b' ';
            length += 1;
        }
        let bytes = &storage[index][..lengths[index]];
        let available = output.len() - length;
        let copied = bytes.len().min(available);
        output[length..length + copied].copy_from_slice(&bytes[..copied]);
        length += copied;
    }
    length
}

/// Os nomes que o shell executa internamente.
fn is_builtin_name(name: &[u8]) -> bool {
    matches!(
        name,
        b"cd" | b"pwd" | b"echo" | b"export" | b"unset" | b"set" | b"exit" | b"logout"
            | b":" | b"true" | b"false" | b"help"
    )
}

fn close_pipes(pipes: &[(usize, usize)]) {
    for &(read_end, write_end) in pipes {
        let _ = fs::close(read_end);
        let _ = fs::close(write_end);
    }
}

/// Pula os pipelines que dependem de `&&`/`||` até o fim da cadeia.
///
/// Uma cadeia termina quando o conector do próximo item não é `&&` nem `||`.
fn skip_alternates(entries: &[Entry], mut index: usize) -> usize {
    while index < entries.len() {
        match entries[index].connector {
            Connector::OnSuccess | Connector::OnFailure => index += 1,
            _ => break,
        }
    }
    index
}

/// Escreve um único campo em um `ExpandedWord` (auxiliar do `cd`).
fn target_push(target: &mut ExpandedWord, bytes: &[u8]) -> Result<(), ()> {
    target.push_field(bytes)
}
