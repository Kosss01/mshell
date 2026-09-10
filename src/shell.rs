use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::builtins;
use crate::executor::{ExecutionResult, Executor};
use crate::parser;

pub fn configure_signal_handling() -> Result<(), String> {
    unsafe {
        if libc::signal(libc::SIGINT, libc::SIG_IGN) == libc::SIG_ERR {
            return Err(format!(
                "failed to ignore SIGINT: {}",
                io::Error::last_os_error()
            ));
        }
        if libc::signal(libc::SIGQUIT, libc::SIG_IGN) == libc::SIG_ERR {
            return Err(format!(
                "failed to ignore SIGQUIT: {}",
                io::Error::last_os_error()
            ));
        }
        if libc::signal(libc::SIGTSTP, libc::SIG_IGN) == libc::SIG_ERR {
            return Err(format!(
                "failed to ignore SIGTSTP: {}",
                io::Error::last_os_error()
            ));
        }
        if libc::signal(libc::SIGTTIN, libc::SIG_IGN) == libc::SIG_ERR {
            return Err(format!(
                "failed to ignore SIGTTIN: {}",
                io::Error::last_os_error()
            ));
        }
        if libc::signal(libc::SIGTTOU, libc::SIG_IGN) == libc::SIG_ERR {
            return Err(format!(
                "failed to ignore SIGTTOU: {}",
                io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

pub struct Shell {
    executor: Executor,
    last_status: i32,
    completion_extractor: crate::completions::HelpCompletionExtractor,
}

impl Shell {
    pub fn new() -> Self {
        Self {
            executor: Executor::persistent(),
            last_status: 0,
            completion_extractor: crate::completions::HelpCompletionExtractor::new(),
        }
    }

    #[allow(dead_code)]
    pub fn ephemeral() -> Self {
        Self {
            executor: Executor::new(),
            last_status: 0,
            completion_extractor: crate::completions::HelpCompletionExtractor::new(),
        }
    }

    #[allow(dead_code)]
    pub fn run(&mut self) -> i32 {
        let args: Vec<String> = env::args().collect();
        self.run_with_args(&args)
    }

    pub fn run_with_args(&mut self, args: &[String]) -> i32 {
        if args.len() > 1 {
            if args[1] == "-c" {
                if args.len() < 3 {
                    eprintln!("mshell: -c: option requires an argument");
                    return 2;
                }
                let mut params = Vec::new();
                if args.len() >= 4 {
                    params.push(args[3].clone());
                    params.extend_from_slice(&args[4..]);
                } else if !args.is_empty() {
                    params.push(args[0].clone());
                }
                parser::set_positional_params(params);
                return self.execute_string(&args[2]);
            }

            let script_idx = if args[1] == "--" {
                if args.len() > 2 {
                    Some(2)
                } else {
                    None
                }
            } else if !args[1].starts_with('-') {
                Some(1)
            } else {
                None
            };

            if let Some(idx) = script_idx {
                let script_path = &args[idx];
                let mut params = vec![script_path.clone()];
                params.extend_from_slice(&args[idx + 1..]);
                parser::set_positional_params(params);
                return self.execute_script(script_path);
            }
        }

        let prog_name = args
            .first()
            .cloned()
            .unwrap_or_else(|| "mshell".to_string());
        parser::set_positional_params(vec![prog_name]);

        if unsafe { libc::isatty(libc::STDIN_FILENO) == 1 } {
            self.run_interactive()
        } else {
            self.run_non_interactive_stdin()
        }
    }

    pub fn execute_string(&mut self, input: &str) -> i32 {
        let ast = match parser::parse_ast(input, self.last_status) {
            Ok(Some(ast)) => ast,
            Ok(None) => return self.last_status,
            Err(error) => {
                eprintln!("mshell: {error}");
                self.last_status = 2;
                return 2;
            }
        };

        let result = self.executor.execute_ast(&ast);
        self.last_status = result.status_code();
        self.last_status
    }

    pub fn execute_script(&mut self, script_path: &str) -> i32 {
        let content = match fs::read_to_string(script_path) {
            Ok(content) => content,
            Err(error) => {
                eprintln!("mshell: {script_path}: {error}");
                return 127;
            }
        };
        self.execute_string(&content)
    }

    pub fn run_non_interactive_stdin(&mut self) -> i32 {
        let mut buffer = String::new();
        match io::stdin().read_to_string(&mut buffer) {
            Ok(_) => self.execute_string(&buffer),
            Err(error) => {
                eprintln!("mshell: failed to read stdin: {error}");
                1
            }
        }
    }

    pub fn run_interactive(&mut self) -> i32 {
        self.executor.set_interactive(true);
        println!("\x1b[1;36m✦ mshell Safe Shell v0.1.0\x1b[0m (type \x1b[1;33mhelp\x1b[0m for builtins, \x1b[1;33mexit\x1b[0m to quit)");
        loop {
            self.print_prompt();

            let input = match self.read_interactive_line() {
                Some(input) => input,
                None => {
                    println!("exit");
                    break;
                }
            };

            let expanded = expand_abbreviations_in_line(&input, &self.executor.abbreviations());
            let final_input = auto_escape_urls(&expanded);

            let ast = match parser::parse_ast(&final_input, self.last_status) {
                Ok(Some(ast)) => ast,
                Ok(None) => continue,
                Err(error) => {
                    eprintln!("mshell: {error}");
                    self.last_status = 2;
                    continue;
                }
            };

            let effects = crate::effects::analyze_ast(&ast);
            if crate::policy::evaluate(&effects) == crate::policy::PolicyDecision::Block {
                eprintln!("mshell: 🛑 Command blocked by safety policy");
                for effect in &effects {
                    eprintln!("- {effect:?}");
                }
                self.last_status = 126;
                continue;
            }

            let result = self.executor.execute_ast(&ast);
            self.last_status = result.status_code();

            if matches!(result, ExecutionResult::Exit(_)) {
                break;
            }
        }

        self.last_status
    }

    fn print_prompt(&self) {
        print!("{}", prompt());
        io::stdout().flush().expect("failed to flush stdout");
    }

    fn read_interactive_line(&self) -> Option<String> {
        let _raw_mode = match RawMode::enable() {
            Ok(mode) => mode,
            Err(error) => {
                eprintln!("mshell: failed to enable line editing: {error}");
                return None;
            }
        };

        let mut line = String::new();
        let mut history_index = self.executor.history().len();
        let mut history_line = String::new();
        let mut suggestion = String::new();
        let mut stdin = io::stdin();

        loop {
            let character = match read_terminal_character(&mut stdin) {
                Ok(Some(character)) => character,
                Ok(None) => return None,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => continue,
            };

            match character {
                '\n' | '\r' => {
                    println!();
                    return Some(line);
                }
                '\u{3}' => {
                    println!("^C");
                    return Some(String::new());
                }
                '\u{4}' if line.is_empty() => return None,
                '\u{12}' => {
                    if let Some(selected) = run_reverse_history_search(&mut stdin, &self.executor.history(), &line) {
                        line = selected;
                        suggestion.clear();
                        redraw_line(&line);
                    }
                }
                '\t' => {
                    if suggestion.is_empty() {
                        complete_line(&mut line, &self.completion_extractor, Some(&self.executor));
                    } else {
                        line.push_str(&suggestion);
                        suggestion.clear();
                        redraw_line(&line);
                    }
                }
                '\u{7f}' | '\u{8}' => {
                    if line.pop().is_some() {
                        suggestion.clear();
                        redraw_line(&line);
                    }
                }
                '\u{1b}' => {
                    let mut b1 = [0; 1];
                    if stdin.read_exact(&mut b1).is_err() {
                        return None;
                    }
                    if b1[0] == b'[' {
                        let mut b2 = [0; 1];
                        if stdin.read_exact(&mut b2).is_err() {
                            return None;
                        }
                        match b2[0] {
                            b'A' => {
                                suggestion.clear();
                                let history = self.executor.history();
                                if history_index > 0 {
                                    history_index -= 1;
                                    history_line = history[history_index].clone();
                                    line.clear();
                                    line.push_str(&history_line);
                                    redraw_line(&line);
                                }
                            }
                            b'B' => {
                                suggestion.clear();
                                let history = self.executor.history();
                                if history_index + 1 < history.len() {
                                    history_index += 1;
                                    history_line = history[history_index].clone();
                                    line.clear();
                                    line.push_str(&history_line);
                                    redraw_line(&line);
                                } else if history_index < history.len() {
                                    history_index = history.len();
                                    history_line.clear();
                                    line.clear();
                                    redraw_line(&line);
                                }
                            }
                            b'C' if !suggestion.is_empty() => {
                                line.push_str(&suggestion);
                                suggestion.clear();
                                redraw_line(&line);
                            }
                            b'2' => {
                                let mut rest = [0; 3];
                                if stdin.read_exact(&mut rest).is_ok() && &rest == b"00~" {
                                    let mut pasted = Vec::new();
                                    loop {
                                        let mut b = [0; 1];
                                        if stdin.read_exact(&mut b).is_err() {
                                            break;
                                        }
                                        if b[0] == 0x1b {
                                            let mut end_seq = [0; 5];
                                            if stdin.read_exact(&mut end_seq).is_ok() && &end_seq == b"[201~" {
                                                break;
                                            }
                                        }
                                        pasted.push(b[0]);
                                    }
                                    let pasted_str = String::from_utf8_lossy(&pasted);
                                    let sanitized = sanitize_pasted_text(&pasted_str);
                                    line.push_str(&sanitized);
                                    suggestion = autosuggestion(&line, &self.executor.history());
                                    redraw_line_with_suggestion(&line, &suggestion);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                character => {
                    if character.is_control() && character != '\t' {
                        continue;
                    }
                    line.push(character);
                    if character == ' ' {
                        line = expand_abbreviations_in_line(&line, &self.executor.abbreviations());
                    }
                    suggestion = autosuggestion(&line, &self.executor.history());
                    redraw_line_with_suggestion(&line, &suggestion);
                }
            }
        }
    }
}

fn read_terminal_character(reader: &mut impl Read) -> io::Result<Option<char>> {
    let mut first = [0; 1];
    loop {
        match reader.read(&mut first) {
            Ok(0) => return Ok(None),
            Ok(1) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
            _ => {}
        }
    }

    let width = match first[0] {
        byte if byte < 0x80 => 1,
        byte if byte & 0xe0 == 0xc0 => 2,
        byte if byte & 0xf0 == 0xe0 => 3,
        byte if byte & 0xf8 == 0xf0 => 4,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid UTF-8 input",
            ));
        }
    };
    let mut bytes = [0; 4];
    bytes[0] = first[0];
    let mut offset = 1;
    while offset < width {
        match reader.read(&mut bytes[offset..width]) {
            Ok(0) => return Ok(None),
            Ok(n) => offset += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    std::str::from_utf8(&bytes[..width])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid UTF-8 input"))?
        .chars()
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty UTF-8 input"))
        .map(Some)
}

fn redraw_line(line: &str) {
    print!("\r\x1b[2K{}{}", prompt(), highlight_line(line));
    io::stdout().flush().ok();
}

fn redraw_line_with_suggestion(line: &str, suggestion: &str) {
    print!(
        "\r\x1b[2K{}{}\x1b[2;37m{}\x1b[0m",
        prompt(),
        highlight_line(line),
        suggestion
    );
    io::stdout().flush().ok();
}

fn prompt() -> String {
    let current = env::current_dir().unwrap_or_else(|_| PathBuf::from("?"));
    let display = env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|home| current.strip_prefix(home).ok().map(Path::to_path_buf))
        .map(|relative| {
            if relative.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~/{}", relative.display())
            }
        })
        .unwrap_or_else(|| current.display().to_string());

    let git_context = git_prompt_context(&current);
    format!("\x1b[1;35mmshell\x1b[0m \x1b[1;34m{display}\x1b[0m{git_context} $ ")
}

fn git_prompt_context(directory: &Path) -> String {
    let mut dir = directory;
    let mut git_dir: Option<PathBuf> = None;

    loop {
        let candidate = dir.join(".git");
        if candidate.is_dir() {
            git_dir = Some(candidate);
            break;
        } else if candidate.is_file() {
            if let Ok(content) = fs::read_to_string(&candidate) {
                if let Some(target) = content.trim().strip_prefix("gitdir:") {
                    let target_path = target.trim();
                    let p = if Path::new(target_path).is_absolute() {
                        PathBuf::from(target_path)
                    } else {
                        dir.join(target_path)
                    };
                    git_dir = Some(p);
                    break;
                }
            }
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }

    let Some(git_path) = git_dir else {
        return String::new();
    };

    let head_path = git_path.join("HEAD");
    let head_content = match fs::read_to_string(head_path) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    let branch = if let Some(ref_path) = head_content.trim().strip_prefix("ref: refs/heads/") {
        ref_path.to_string()
    } else {
        head_content.trim().chars().take(7).collect()
    };

    if branch.is_empty() {
        return String::new();
    }

    format!(" \x1b[1;35m({branch})\x1b[0m")
}

fn autosuggestion(line: &str, history: &[String]) -> String {
    if line.is_empty() {
        return String::new();
    }
    history
        .iter()
        .rev()
        .find(|command| command.starts_with(line) && command.len() > line.len())
        .map(|command| command[line.len()..].to_string())
        .unwrap_or_default()
}

fn highlight_line(line: &str) -> String {
    const RESET: &str = "\x1b[0m";
    const COMMAND: &str = "\x1b[1;36m";
    const BUILTIN: &str = "\x1b[1;32m";
    const QUOTE: &str = "\x1b[33m";
    const VARIABLE: &str = "\x1b[35m";

    let mut highlighted = String::new();
    let mut token = String::new();
    let mut first_token = true;
    let mut chars = line.chars().peekable();

    let flush_token = |highlighted: &mut String, token: &mut String, first: &mut bool| {
        if token.is_empty() {
            return;
        }
        let style = if *first {
            if is_builtin_name(token) {
                BUILTIN
            } else {
                COMMAND
            }
        } else {
            ""
        };
        highlighted.push_str(style);
        highlighted.push_str(token);
        if !style.is_empty() {
            highlighted.push_str(RESET);
        }
        token.clear();
        *first = false;
    };

    while let Some(character) = chars.next() {
        if character == '\\' {
            token.push(character);
            if let Some(escaped) = chars.next() {
                token.push(escaped);
            }
            continue;
        }
        if character.is_whitespace() {
            flush_token(&mut highlighted, &mut token, &mut first_token);
            highlighted.push(character);
            continue;
        }
        if matches!(character, '|' | '<' | '>' | '&' | ';') {
            flush_token(&mut highlighted, &mut token, &mut first_token);
            highlighted.push_str("\x1b[1;33m");
            highlighted.push(character);
            highlighted.push_str(RESET);
            continue;
        }
        if character == '\'' || character == '"' {
            flush_token(&mut highlighted, &mut token, &mut first_token);
            let quote = character;
            highlighted.push_str(QUOTE);
            highlighted.push(quote);
            while let Some(next) = chars.next() {
                highlighted.push(next);
                if next == quote {
                    break;
                }
            }
            highlighted.push_str(RESET);
            first_token = false;
            continue;
        }
        if character == '$' {
            flush_token(&mut highlighted, &mut token, &mut first_token);
            highlighted.push_str(VARIABLE);
            highlighted.push(character);
            while let Some(next) = chars.peek().copied() {
                if next.is_ascii_alphanumeric() || matches!(next, '_' | '?' | '{' | '}') {
                    highlighted.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            highlighted.push_str(RESET);
            continue;
        }
        token.push(character);
    }
    flush_token(&mut highlighted, &mut token, &mut first_token);
    highlighted
}

fn is_builtin_name(name: &str) -> bool {
    builtins::is_builtin(name)
}

fn complete_line(
    line: &mut String,
    extractor: &crate::completions::HelpCompletionExtractor,
    executor: Option<&Executor>,
) {
    let start = line
        .char_indices()
        .rev()
        .find(|(_, character)| character.is_whitespace())
        .map(|(index, _)| index + 1)
        .unwrap_or(0);
    let prefix = &line[start..];
    let candidates = if command_completion_position(&line[..start]) {
        let extra = match executor {
            Some(exec) => {
                let mut names: Vec<String> = exec.aliases().keys().cloned().collect();
                names.extend(exec.functions().keys().cloned());
                names
            }
            None => Vec::new(),
        };
        command_candidates(prefix, &extra)
    } else if prefix.starts_with('-') {
        let cmd = find_command_for_flag_completion(&line[..start]);
        let flag_comps = if let Some(cmd) = cmd {
            extractor.complete_flags(&cmd, prefix)
        } else {
            Vec::new()
        };
        if flag_comps.is_empty() {
            path_candidates(prefix)
        } else {
            flag_comps.into_iter().map(|c| c.flag).collect()
        }
    } else {
        path_candidates(prefix)
    };

    match candidates.as_slice() {
        [candidate] => {
            line.truncate(start);
            line.push_str(candidate);
            redraw_line(line);
        }
        [] => {}
        candidates => {
            print!("\r\n{}\r\n", candidates.join("  "));
            redraw_line(line);
        }
    }
}

fn find_command_for_flag_completion(before_cursor: &str) -> Option<String> {
    let segment = before_cursor.rsplit('|').next().unwrap_or_default().trim();
    segment
        .split_whitespace()
        .find(|word| !word.contains('='))
        .map(|w| w.to_string())
}

fn command_completion_position(before_cursor: &str) -> bool {
    let context = before_cursor.trim_end();
    if context.is_empty() || context.ends_with('|') {
        return true;
    }

    let command_segment = context.rsplit('|').next().unwrap_or_default().trim();
    if command_segment.is_empty() {
        return true;
    }

    command_segment
        .split_whitespace()
        .all(|word| word.split_once('=').is_some_and(|(name, _)| is_valid_environment_name(name)))
}

fn is_valid_environment_name(name: &str) -> bool {
    let mut characters = name.chars();
    matches!(characters.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn command_candidates(prefix: &str, extra: &[String]) -> Vec<String> {
    let mut candidates = BTreeSet::new();
    for command in builtins::BUILTIN_NAMES {
        if command.starts_with(prefix) {
            candidates.insert(command.to_string());
        }
    }

    for name in extra {
        if name.starts_with(prefix) {
            candidates.insert(name.clone());
        }
    }

    if let Some(paths) = env::var_os("PATH") {
        for directory in env::split_paths(&paths) {
            let entries = match fs::read_dir(directory) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with(prefix) && is_executable(&entry.path()) {
                    candidates.insert(name);
                }
            }
        }
    }
    candidates.into_iter().collect()
}

fn path_candidates(prefix: &str) -> Vec<String> {
    let (directory, name_prefix) = match prefix.rsplit_once('/') {
        Some((directory, name)) => {
            let directory = if directory.is_empty() { "/" } else { directory };
            (PathBuf::from(directory), name)
        }
        None => (PathBuf::from("."), prefix),
    };
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(name_prefix) {
                return None;
            }
            let mut candidate = if prefix.contains('/') {
                format!("{}/{}", prefix.rsplit_once('/').unwrap().0, name)
            } else {
                name
            };
            if entry.path().is_dir() {
                candidate.push('/');
            }
            Some(candidate)
        })
        .collect()
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

struct RawMode {
    original: libc::termios,
}

impl RawMode {
    fn enable() -> io::Result<Self> {
        let fd = libc::STDIN_FILENO;
        let mut original = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut original) } == -1 {
            return Err(io::Error::last_os_error());
        }

        let mut raw = original;
        unsafe {
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(fd, libc::TCSANOW, &raw) == -1 {
                return Err(io::Error::last_os_error());
            }
        }
        print!("\x1b[?2004h");
        io::stdout().flush().ok();
        Ok(Self { original })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        print!("\x1b[?2004l");
        io::stdout().flush().ok();
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original);
        }
    }
}

pub fn sanitize_pasted_text(input: &str) -> String {
    let mut result = String::new();
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&']') {
                chars.next();
                while let Some(sc) = chars.next() {
                    if sc == '\x07' {
                        break;
                    }
                    if sc == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
                continue;
            }
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&sc) = chars.peek() {
                    chars.next();
                    if ('\x40'..='\x7e').contains(&sc) {
                        break;
                    }
                }
                continue;
            }
            continue;
        }

        if c == '\r' || c == '\n' {
            let mut has_more = false;
            while let Some(&next_c) = chars.peek() {
                if next_c == '\r' || next_c == '\n' {
                    chars.next();
                } else {
                    has_more = true;
                    break;
                }
            }
            if has_more && !result.is_empty() && !result.ends_with("; ") && !result.ends_with(' ') {
                result.push_str("; ");
            }
            continue;
        }

        if c.is_control() && c != '\t' {
            continue;
        }

        result.push(c);
    }

    let trimmed = result.trim_end();
    if let Some(stripped) = trimmed.strip_suffix(';') {
        stripped.trim_end().to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn auto_escape_urls(line: &str) -> String {
    let mut result = String::new();
    let mut chars = line.chars().peekable();
    let mut quote: Option<char> = None;
    let mut word = String::new();

    let flush_word = |result: &mut String, word: &mut String| {
        if word.starts_with("http://") || word.starts_with("https://") {
            let mut escaped_word = String::new();
            let mut prev_backslash = false;
            for c in word.chars() {
                if (c == '?' || c == '&') && !prev_backslash {
                    escaped_word.push('\\');
                }
                escaped_word.push(c);
                prev_backslash = c == '\\' && !prev_backslash;
            }
            result.push_str(&escaped_word);
        } else {
            result.push_str(word);
        }
        word.clear();
    };

    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            result.push(c);
            continue;
        }

        if c == '\'' || c == '"' {
            flush_word(&mut result, &mut word);
            quote = Some(c);
            result.push(c);
            continue;
        }

        if c.is_whitespace() || c == '|' || c == ';' || c == '(' || c == ')' {
            flush_word(&mut result, &mut word);
            result.push(c);
            continue;
        }

        word.push(c);
    }
    flush_word(&mut result, &mut word);
    result
}

pub fn expand_abbreviations_in_line(
    line: &str,
    abbreviations: &std::collections::BTreeMap<String, String>,
) -> String {
    if abbreviations.is_empty() {
        return line.to_string();
    }

    let mut result = String::new();
    let mut remaining = line;

    while !remaining.is_empty() {
        let (segment, sep, next_remaining) = match find_command_separator(remaining) {
            Some((start, end)) => (
                &remaining[..start],
                &remaining[start..end],
                &remaining[end..],
            ),
            None => (remaining, "", ""),
        };

        result.push_str(&expand_segment_abbreviation(segment, abbreviations));
        result.push_str(sep);
        remaining = next_remaining;
    }

    result
}

fn find_command_separator(s: &str) -> Option<(usize, usize)> {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let bytes = s.as_bytes();

    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        if b == b'\\' && !in_single {
            escaped = true;
            i += 1;
            continue;
        }
        if b == b'\'' && !in_double {
            in_single = !in_single;
            i += 1;
            continue;
        }
        if b == b'"' && !in_single {
            in_double = !in_double;
            i += 1;
            continue;
        }

        if !in_single && !in_double {
            if s[i..].starts_with("&&") {
                return Some((i, i + 2));
            }
            if s[i..].starts_with("||") {
                return Some((i, i + 2));
            }
            if s[i..].starts_with("|>") {
                return Some((i, i + 2));
            }
            if b == b'|' {
                return Some((i, i + 1));
            }
            if b == b';' {
                return Some((i, i + 1));
            }
        }
        i += 1;
    }
    None
}

fn expand_segment_abbreviation(
    segment: &str,
    abbreviations: &std::collections::BTreeMap<String, String>,
) -> String {
    let leading_len = segment.len() - segment.trim_start().len();
    let (leading_ws, rest) = segment.split_at(leading_len);

    let parts: Vec<&str> = rest.split_whitespace().collect();
    let mut assign_count = 0;
    for part in &parts {
        if part.contains('=') && !part.starts_with('=') {
            assign_count += 1;
        } else {
            break;
        }
    }

    if assign_count < parts.len() {
        let cmd = parts[assign_count];
        if let Some(expansion) = abbreviations.get(cmd) {
            let mut search_start = 0;
            for _ in 0..assign_count {
                if let Some(pos) = rest[search_start..].find('=') {
                    let next_space = rest[search_start + pos..]
                        .find(char::is_whitespace)
                        .unwrap_or(rest.len() - (search_start + pos));
                    search_start = search_start + pos + next_space;
                }
            }
            if let Some(cmd_pos) = rest[search_start..].find(cmd) {
                let actual_cmd_pos = search_start + cmd_pos;
                let before_cmd = &rest[..actual_cmd_pos];
                let after_cmd = &rest[actual_cmd_pos + cmd.len()..];
                return format!("{leading_ws}{before_cmd}{expansion}{after_cmd}");
            }
        }
    }

    segment.to_string()
}

pub fn fuzzy_match(pattern: &str, candidate: &str) -> bool {
    let pattern = pattern.to_lowercase();
    let candidate = candidate.to_lowercase();
    let mut pat_chars = pattern.chars().peekable();
    for c in candidate.chars() {
        if let Some(&p) = pat_chars.peek() {
            if c == p {
                pat_chars.next();
            }
        } else {
            break;
        }
    }
    pat_chars.peek().is_none()
}

fn run_reverse_history_search(
    stdin: &mut io::Stdin,
    history: &[String],
    initial_line: &str,
) -> Option<String> {
    let mut query = String::new();
    let mut match_idx: Option<usize> = None;

    let find_match = |query: &str, hist: &[String], from: usize| -> Option<usize> {
        if query.is_empty() {
            return None;
        }
        for (i, item) in hist[..from].iter().enumerate().rev() {
            if fuzzy_match(query, item) {
                return Some(i);
            }
        }
        None
    };

    draw_search_prompt(&query, match_idx.and_then(|i| history.get(i).map(|s| s.as_str())));

    loop {
        let character = match read_terminal_character(stdin) {
            Ok(Some(c)) => c,
            _ => return None,
        };

        match character {
            '\u{12}' => {
                if let Some(current) = match_idx {
                    if current > 0 {
                        if let Some(prev) = find_match(&query, history, current) {
                            match_idx = Some(prev);
                        } else {
                            match_idx = find_match(&query, history, history.len());
                        }
                    }
                }
            }
            '\n' | '\r' => {
                println!();
                return match_idx
                    .and_then(|i| history.get(i).cloned())
                    .or_else(|| Some(initial_line.to_string()));
            }
            '\t' => {
                let selected = match_idx
                    .and_then(|i| history.get(i).cloned())
                    .unwrap_or_else(|| initial_line.to_string());
                redraw_line(&selected);
                return Some(selected);
            }
            '\u{3}' | '\u{1b}' => {
                redraw_line(initial_line);
                return Some(initial_line.to_string());
            }
            '\u{7f}' | '\u{8}' => {
                query.pop();
                match_idx = find_match(&query, history, history.len());
            }
            c if !c.is_control() => {
                query.push(c);
                match_idx = find_match(&query, history, history.len());
            }
            _ => {}
        }

        draw_search_prompt(&query, match_idx.and_then(|i| history.get(i).map(|s| s.as_str())));
    }
}

fn draw_search_prompt(query: &str, matched: Option<&str>) {
    let matched_str = matched.unwrap_or("");
    print!(
        "\r\x1b[2K(reverse-i-search)`\x1b[1;33m{}\x1b[0m': {}",
        query, matched_str
    );
    io::stdout().flush().ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn completes_builtin_prefixes() {
        let candidates = command_candidates("his", &[]);

        assert!(candidates.contains(&"history".to_string()));
    }

    #[test]
    fn completes_aliases_and_functions() {
        let extra = vec!["greet".to_string(), "gstatus".to_string()];
        let candidates = command_candidates("g", &extra);
        assert!(candidates.contains(&"greet".to_string()));
        assert!(candidates.contains(&"gstatus".to_string()));
    }

    #[test]
    fn completion_and_highlighting_include_all_builtins() {
        let candidates = command_candidates("exp", &[]);
        assert!(candidates.contains(&"export".to_string()));

        let highlighted = highlight_line("printf '%s' value");
        assert!(highlighted.contains("\x1b[1;32mprintf\x1b[0m"));
    }

    #[test]
    fn recognizes_command_positions_after_pipelines_and_assignments() {
        assert!(command_completion_position(""));
        assert!(command_completion_position("echo value | "));
        assert!(command_completion_position("MODE=test "));
        assert!(!command_completion_position("echo "));
    }

    #[test]
    fn completes_existing_relative_paths() {
        let candidates = path_candidates("Cargo.");

        assert!(candidates.contains(&"Cargo.toml".to_string()));
    }

    #[test]
    fn highlights_builtins_variables_and_operators() {
        let highlighted = highlight_line("echo $HOME | wc");

        assert!(highlighted.contains("\x1b[1;32mecho\x1b[0m"));
        assert!(highlighted.contains("\x1b[35m$HOME\x1b[0m"));
        assert!(highlighted.contains("\x1b[1;33m|\x1b[0m"));
    }

    #[test]
    fn does_not_highlight_escaped_variables_as_expansions() {
        let highlighted = highlight_line(r"echo \$HOME");

        assert!(!highlighted.contains("\x1b[35m$HOME"));
        assert!(highlighted.contains(r"\$HOME"));
    }

    #[test]
    fn decodes_utf8_terminal_characters() {
        let mut input = Cursor::new("é".as_bytes());

        assert_eq!(read_terminal_character(&mut input).unwrap(), Some('é'));
    }

    #[test]
    fn preserves_ascii_control_characters() {
        let mut input = Cursor::new([3_u8]);

        assert_eq!(read_terminal_character(&mut input).unwrap(), Some('\u{3}'));
    }

    #[test]
    fn suggests_latest_matching_history_entry() {
        let history = vec!["echo first".into(), "echo second".into()];

        assert_eq!(autosuggestion("echo ", &history), "second");
        assert_eq!(autosuggestion("pwd", &history), "");
    }

    #[test]
    fn abbreviates_home_in_prompt() {
        let prompt = prompt();

        assert!(prompt.contains(" $ "));
        assert!(prompt.contains("\x1b[1;34m"));
    }

    #[test]
    fn omits_git_context_outside_a_repository() {
        assert_eq!(git_prompt_context(Path::new("/tmp")), "");
    }

    #[test]
    fn executes_string_commands() {
        let mut shell = Shell::ephemeral();
        assert_eq!(shell.execute_string("true"), 0);
        assert_eq!(shell.execute_string("false"), 1);
        assert_eq!(shell.execute_string("echo ok && true"), 0);
    }

    #[test]
    fn executes_dash_c_with_args() {
        let mut shell = Shell::ephemeral();
        let args = vec![
            "mshell".to_string(),
            "-c".to_string(),
            "echo $1 $2".to_string(),
            "my_script".to_string(),
            "hello".to_string(),
            "world".to_string(),
        ];
        assert_eq!(shell.run_with_args(&args), 0);
        assert_eq!(
            parser::get_positional_params(),
            vec!["my_script", "hello", "world"]
        );
    }

    #[test]
    fn executes_script_file() {
        let dir = std::env::temp_dir();
        let script_path = dir.join("mshell_test_script.sh");
        std::fs::write(&script_path, "echo foo\ntrue\n").unwrap();

        let mut shell = Shell::ephemeral();
        let args = vec![
            "mshell".to_string(),
            script_path.to_str().unwrap().to_string(),
        ];
        assert_eq!(shell.run_with_args(&args), 0);
        let _ = std::fs::remove_file(script_path);
    }

    #[test]
    fn sanitizes_pasted_text_and_neutralizes_newlines() {
        let malicious = "git status\nrm -rf / --no-preserve-root\n\x1b]52;c;evil\x07";
        let sanitized = sanitize_pasted_text(malicious);
        assert_eq!(sanitized, "git status; rm -rf / --no-preserve-root");
    }

    #[test]
    fn auto_escapes_unquoted_urls() {
        let input = "curl https://api.example.com/items?limit=10&page=2";
        let escaped = auto_escape_urls(input);
        assert_eq!(escaped, r"curl https://api.example.com/items\?limit=10\&page=2");

        let quoted = r#"curl "https://api.example.com/items?limit=10&page=2""#;
        assert_eq!(auto_escape_urls(quoted), quoted);
    }

    #[test]
    fn expands_abbreviations_in_command_position() {
        let mut map = std::collections::BTreeMap::new();
        map.insert("gco".into(), "git checkout".into());
        map.insert("gst".into(), "git status".into());

        assert_eq!(expand_abbreviations_in_line("gco", &map), "git checkout");
        assert_eq!(expand_abbreviations_in_line("gco main", &map), "git checkout main");
        assert_eq!(expand_abbreviations_in_line("echo gco", &map), "echo gco");
        assert_eq!(expand_abbreviations_in_line("gco && gst", &map), "git checkout && git status");
    }

    #[test]
    fn fuzzy_matches_subsequences() {
        assert!(fuzzy_match("gco", "git checkout"));
        assert!(fuzzy_match("dock", "docker run -it"));
        assert!(!fuzzy_match("xyz", "docker run"));
    }
}
