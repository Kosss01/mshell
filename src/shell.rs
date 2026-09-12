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

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
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
        let mut is_tutor = false;
        let mut is_sandbox = false;
        let mut is_plain = false;
        let mut c_opt = None;
        let mut script_opt = None;

        let mut i = 1;
        while i < args.len() {
            if args[i] == "--help" || args[i] == "-h" {
                print_cli_help();
                return 0;
            } else if args[i] == "--version" || args[i] == "-v" || args[i] == "-V" {
                println!("shellpilot {}", env!("CARGO_PKG_VERSION"));
                return 0;
            } else if args[i] == "--tutor" {
                is_tutor = true;
                i += 1;
            } else if args[i] == "--sandbox" {
                is_sandbox = true;
                i += 1;
            } else if args[i] == "--plain" || args[i] == "--no-tutor" {
                is_plain = true;
                i += 1;
            } else if args[i] == "-c" {
                if i + 1 >= args.len() {
                    eprintln!("shellpilot: -c: option requires an argument");
                    return 2;
                }
                c_opt = Some(i + 1);
                break;
            } else if args[i] == "--" {
                if i + 1 < args.len() {
                    script_opt = Some(i + 1);
                }
                break;
            } else if !args[i].starts_with('-') {
                script_opt = Some(i);
                break;
            } else {
                eprintln!("shellpilot: unrecognized option: {}", args[i]);
                return 2;
            }
        }

        let is_interactive = unsafe { libc::isatty(libc::STDIN_FILENO) == 1 };

        // By default in interactive mode (e.g. cargo run), launch directly into Flight Simulator Tutor
        if is_interactive && c_opt.is_none() && script_opt.is_none() && !is_plain && !is_sandbox {
            is_tutor = true;
        }

        if is_tutor || is_sandbox {
            self.executor.set_interactive(true);
            if is_tutor {
                self.executor.ensure_tutor();
            }
            match self.executor.ensure_sandbox() {
                Ok(ws) => {
                    let _ = std::env::set_current_dir(&ws);
                    if is_interactive && c_opt.is_none() && script_opt.is_none() {
                        if is_tutor {
                            let target_lesson = {
                                let t = self.executor.tutor.borrow();
                                let tutor = t.as_ref().unwrap();
                                tutor
                                    .first_incomplete_lesson()
                                    .map(|l| l.id)
                                    .unwrap_or("nav_01")
                                    .to_string()
                            };

                            println!("\x1b[1;36m╔══════════════════════════════════════════════════════════════════════════╗\x1b[0m");
                            println!("\x1b[1;36m║            ✈️   SHELLPILOT COMMAND LINE FLIGHT SIMULATOR                 ║\x1b[0m");
                            println!("\x1b[1;36m║                   Interactive Academy & Safety Sandbox                   ║\x1b[0m");
                            println!("\x1b[1;36m╚══════════════════════════════════════════════════════════════════════════╝\x1b[0m\n");

                            println!("👋 \x1b[1;32mWelcome, Cadet!\x1b[0m You are in a safe, isolated virtual server sandbox:");
                            println!("   • \x1b[1;34mEnvironment\x1b[0m : ~/lab (pre-loaded with app/, config/, logs/, data/, scripts/)");
                            println!("   • \x1b[1;34mSafety Net \x1b[0m : Commands are jailed and cannot harm your host system");
                            println!("   • \x1b[1;34mTime-Travel\x1b[0m : Type '\x1b[1;33mundo\x1b[0m' (or '\x1b[1;33mundo diff\x1b[0m'), or '\x1b[1;33mwhatif <cmd>\x1b[0m' to preview");
                            println!("   • \x1b[1;34mToolbox    \x1b[0m : '\x1b[1;33mtree\x1b[0m' for folder map, '\x1b[1;33mcheat <tool>\x1b[0m' for cheatsheets, '\x1b[1;33mdoctor\x1b[0m' for fixes");
                            println!("   • \x1b[1;34mOperations \x1b[0m : '\x1b[1;33mservice\x1b[0m' (start/status), '\x1b[1;33mcurl\x1b[0m' APIs, '\x1b[1;33mdrill\x1b[0m' outages, '\x1b[1;33mcadet\x1b[0m' dossier\n");

                            println!("📋 \x1b[1;35mGetting Started:\x1b[0m");
                            println!("   1. Read the \x1b[1mMission Objective\x1b[0m below and type your command at the prompt.");
                            println!("   2. Your solution is \x1b[1mauto-validated\x1b[0m immediately upon execution.");
                            println!("   3. Need help? Type '\x1b[1;33mtutor hint\x1b[0m' for hints or '\x1b[1;33mtutor solution\x1b[0m' for answers.");
                            println!("   4. Type '\x1b[1;33mtutor\x1b[0m' to see all 7 tracks & 26 challenges, or '\x1b[1;33mtutor next\x1b[0m' to advance.\n");

                            let briefing = {
                                let mut tutor = self.executor.tutor.borrow_mut();
                                tutor
                                    .as_mut()
                                    .unwrap()
                                    .start_lesson(&target_lesson, &ws)
                                    .unwrap_or_default()
                            };
                            println!("{briefing}");
                            set_prompt_prefix(Some(format!("shellpilot:tutor 🎓 {target_lesson}")));
                        } else {
                            println!("\n🛡️  [SHELLPILOT FLIGHT SIMULATOR: SANDBOX ACTIVE]");
                            println!("   Confinement root: {}", ws.display());
                            println!("   All commands are safely isolated. Try 'undo' or 'whatif <cmd>'.\n");
                            println!("📁 A realistic virtual server environment is ready in ~/lab:");
                            println!("   • app/     : Python service, config.json, HTML templates");
                            println!("   • config/  : server.conf, database.yaml, settings.env");
                            println!("   • logs/    : access.log, error.log, auth.log");
                            println!("   • data/    : customers.csv, inventory.jsonl");
                            println!("   • scripts/ : deploy.sh, backup.sh, healthcheck.sh\n");
                            println!("💡 Try: 'ls -la', 'cat config/server.conf', 'grep ERROR logs/error.log'\n");
                            set_prompt_prefix(Some("shellpilot:sandbox 🛡️".into()));
                        }
                    }
                }
                Err(err) => {
                    eprintln!("shellpilot: failed to initialize sandbox: {err}");
                    return 1;
                }
            }
        }

        if let Some(cmd_idx) = c_opt {
            let mut params = Vec::new();
            if cmd_idx + 1 < args.len() {
                params.push(args[cmd_idx + 1].clone());
                params.extend_from_slice(&args[cmd_idx + 2..]);
            } else if !args.is_empty() {
                params.push(args[0].clone());
            }
            parser::set_positional_params(params);
            return self.execute_string(&args[cmd_idx]);
        }

        if let Some(idx) = script_opt {
            let script_path = &args[idx];
            let mut params = vec![script_path.clone()];
            params.extend_from_slice(&args[idx + 1..]);
            parser::set_positional_params(params);
            return self.execute_script(script_path);
        }

        let prog_name = args
            .first()
            .cloned()
            .unwrap_or_else(|| "shellpilot".to_string());
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
                eprintln!("shellpilot: {error}");
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
                eprintln!("shellpilot: {script_path}: {error}");
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
                eprintln!("shellpilot: failed to read stdin: {error}");
                1
            }
        }
    }

    pub fn run_interactive(&mut self) -> i32 {
        self.executor.set_interactive(true);
        println!("\x1b[1;36m✈️  ShellPilot Command Line Flight Simulator v0.1.0\x1b[0m (type \x1b[1;33mhelp\x1b[0m for builtins, \x1b[1;33mexit\x1b[0m to quit)");
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
                    eprintln!("shellpilot: {error}");
                    self.last_status = 2;
                    continue;
                }
            };

            let lint_warnings = crate::linter::check_ast(&ast);
            for w in lint_warnings {
                println!("{}", w.format());
            }

            let effects = crate::effects::analyze_ast(&ast);
            if crate::policy::evaluate(&effects) == crate::policy::PolicyDecision::Block {
                eprintln!("shellpilot: 🛑 Command blocked by safety policy");
                for effect in &effects {
                    eprintln!("- {effect:?}");
                }
                self.last_status = 126;
                continue;
            }

            // Sandbox path confinement and pre-command snapshot
            let mut path_blocked = false;
            if let Some(ref mut sb) = *self.executor.sandbox.borrow_mut() {
                for effect in &effects {
                    match effect {
                        crate::effects::Effect::FilesystemWrite(p) | crate::effects::Effect::FilesystemDelete(p)
                            if !sb.is_path_jailed(std::path::Path::new(p), true) => {
                                eprintln!("shellpilot: 🛡️ Sandbox Guard: Prevented modification to external path '{p}'.");
                                path_blocked = true;
                                break;
                            }
                        _ => {}
                    }
                }

                if !path_blocked {
                    let modifies = effects.iter().any(|e| {
                        matches!(
                            e,
                            crate::effects::Effect::FilesystemDelete(_)
                                | crate::effects::Effect::FilesystemWrite(_)
                        )
                    });
                    if modifies {
                        sb.pre_command_snapshot(&final_input).ok();
                    }
                }
            }

            if path_blocked {
                self.last_status = 1;
                continue;
            }

            let result = self.executor.execute_ast(&ast);
            self.last_status = result.status_code();

            self.executor.record_cadet_command();

            if self.last_status != 0 {
                *self.executor.last_failed_cmd.borrow_mut() = Some((final_input.clone(), self.last_status));
                if self
                    .executor
                    .tutor
                    .borrow()
                    .as_ref()
                    .map(|t| t.active_lesson_id.is_some())
                    .unwrap_or(false)
                {
                    let ws = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                    if let Some(hint) = crate::guidance::suggest_inline_hint(&final_input, self.last_status, &ws) {
                        println!("{hint}");
                    }
                }
            } else {
                *self.executor.last_failed_cmd.borrow_mut() = None;
            }

            // Tutor auto-evaluation hook
            let completed_lesson = {
                let tutor_borrow = self.executor.tutor.borrow();
                if let Some(ref tutor) = *tutor_borrow {
                    if tutor.active_lesson_id.is_some() {
                        let ws = self.executor.current_workspace();
                        if let Some(crate::tutor::ValidationResult::Success { feedback }) =
                            tutor.evaluate_current(&ws, Some(&final_input))
                        {
                            println!("\n🎉 [OBJECTIVE COMPLETE!] {feedback}");
                            println!("   Type 'tutor next' to advance to the next challenge!\n");
                            tutor.active_lesson_id.clone()
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            if let Some(id) = completed_lesson {
                self.executor.mark_tutor_lesson_completed(&id);
            }

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
                eprintln!("shellpilot: failed to enable line editing: {error}");
                return None;
            }
        };

        let mut line = String::new();
        let mut cursor_pos = 0;
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
                '\u{4}' => {
                    if line.is_empty() {
                        return None;
                    }
                    if cursor_pos < line.chars().count() {
                        remove_char_at(&mut line, cursor_pos);
                        if cursor_pos == line.chars().count() {
                            suggestion = autosuggestion(&line, &self.executor.history());
                        } else {
                            suggestion.clear();
                        }
                        redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                    }
                }
                '\u{1}' => {
                    cursor_pos = 0;
                    suggestion.clear();
                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                }
                '\u{5}' => {
                    cursor_pos = line.chars().count();
                    suggestion = autosuggestion(&line, &self.executor.history());
                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                }
                '\u{b}' => {
                    let start_byte = char_to_byte_index(&line, cursor_pos);
                    line.truncate(start_byte);
                    suggestion = autosuggestion(&line, &self.executor.history());
                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                }
                '\u{15}' => {
                    if cursor_pos > 0 {
                        let end_byte = char_to_byte_index(&line, cursor_pos);
                        line.drain(..end_byte);
                        cursor_pos = 0;
                        if cursor_pos == line.chars().count() {
                            suggestion = autosuggestion(&line, &self.executor.history());
                        } else {
                            suggestion.clear();
                        }
                        redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                    }
                }
                '\u{17}' => {
                    if cursor_pos > 0 {
                        let prev_pos = prev_word_boundary(&line, cursor_pos);
                        let start_byte = char_to_byte_index(&line, prev_pos);
                        let end_byte = char_to_byte_index(&line, cursor_pos);
                        line.drain(start_byte..end_byte);
                        cursor_pos = prev_pos;
                        if cursor_pos == line.chars().count() {
                            suggestion = autosuggestion(&line, &self.executor.history());
                        } else {
                            suggestion.clear();
                        }
                        redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                    }
                }
                '\u{c}' => {
                    print!("\x1b[2J\x1b[H");
                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                }
                '\u{12}' => {
                    if let Some(selected) = run_reverse_history_search(&mut stdin, &self.executor.history(), &line) {
                        line = selected;
                        cursor_pos = line.chars().count();
                        suggestion.clear();
                        redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                    }
                }
                '\t' => {
                    if suggestion.is_empty() {
                        complete_line(&mut line, &self.completion_extractor, Some(&self.executor));
                        cursor_pos = line.chars().count();
                    } else {
                        line.push_str(&suggestion);
                        cursor_pos = line.chars().count();
                        suggestion.clear();
                        redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                    }
                }
                '\u{7f}' | '\u{8}' => {
                    if cursor_pos > 0 {
                        cursor_pos -= 1;
                        remove_char_at(&mut line, cursor_pos);
                        if cursor_pos == line.chars().count() {
                            suggestion = autosuggestion(&line, &self.executor.history());
                        } else {
                            suggestion.clear();
                        }
                        redraw_line_with_cursor(&line, &suggestion, cursor_pos);
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
                                    cursor_pos = line.chars().count();
                                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
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
                                    cursor_pos = line.chars().count();
                                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                                } else if history_index < history.len() {
                                    history_index = history.len();
                                    history_line.clear();
                                    line.clear();
                                    cursor_pos = 0;
                                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                                }
                            }
                            b'C' => {
                                if cursor_pos < line.chars().count() {
                                    cursor_pos += 1;
                                    if cursor_pos == line.chars().count() {
                                        suggestion = autosuggestion(&line, &self.executor.history());
                                    } else {
                                        suggestion.clear();
                                    }
                                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                                } else if !suggestion.is_empty() {
                                    line.push_str(&suggestion);
                                    cursor_pos = line.chars().count();
                                    suggestion.clear();
                                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                                }
                            }
                            b'D' => {
                                if cursor_pos > 0 {
                                    cursor_pos -= 1;
                                    suggestion.clear();
                                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                                }
                            }
                            b'H' => {
                                cursor_pos = 0;
                                suggestion.clear();
                                redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                            }
                            b'F' => {
                                cursor_pos = line.chars().count();
                                suggestion = autosuggestion(&line, &self.executor.history());
                                redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                            }
                            b'3' => {
                                let mut b3 = [0; 1];
                                if stdin.read_exact(&mut b3).is_ok() && b3[0] == b'~' && cursor_pos < line.chars().count() {
                                    remove_char_at(&mut line, cursor_pos);
                                    if cursor_pos == line.chars().count() {
                                        suggestion = autosuggestion(&line, &self.executor.history());
                                    } else {
                                        suggestion.clear();
                                    }
                                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                                }
                            }
                            b'1' => {
                                let mut b3 = [0; 1];
                                if stdin.read_exact(&mut b3).is_ok() {
                                    if b3[0] == b'~' {
                                        cursor_pos = 0;
                                        suggestion.clear();
                                        redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                                    } else if b3[0] == b';' {
                                        let mut b_mod_dir = [0; 2];
                                        if stdin.read_exact(&mut b_mod_dir).is_ok() {
                                            match b_mod_dir[1] {
                                                b'D' => {
                                                    cursor_pos = prev_word_boundary(&line, cursor_pos);
                                                    suggestion.clear();
                                                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                                                }
                                                b'C' => {
                                                    cursor_pos = next_word_boundary(&line, cursor_pos);
                                                    if cursor_pos == line.chars().count() {
                                                        suggestion = autosuggestion(&line, &self.executor.history());
                                                    } else {
                                                        suggestion.clear();
                                                    }
                                                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                }
                            }
                            b'4' | b'8' => {
                                let mut b3 = [0; 1];
                                let _ = stdin.read_exact(&mut b3);
                                cursor_pos = line.chars().count();
                                suggestion = autosuggestion(&line, &self.executor.history());
                                redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                            }
                            b'7' => {
                                let mut b3 = [0; 1];
                                let _ = stdin.read_exact(&mut b3);
                                cursor_pos = 0;
                                suggestion.clear();
                                redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                            }
                            b'5' => {
                                let mut b3 = [0; 1];
                                if stdin.read_exact(&mut b3).is_ok() {
                                    if b3[0] == b'D' {
                                        cursor_pos = prev_word_boundary(&line, cursor_pos);
                                        suggestion.clear();
                                        redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                                    } else if b3[0] == b'C' {
                                        cursor_pos = next_word_boundary(&line, cursor_pos);
                                        if cursor_pos == line.chars().count() {
                                            suggestion = autosuggestion(&line, &self.executor.history());
                                        } else {
                                            suggestion.clear();
                                        }
                                        redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                                    }
                                }
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
                                    let pasted_count = sanitized.chars().count();
                                    insert_str_at(&mut line, cursor_pos, &sanitized);
                                    cursor_pos += pasted_count;
                                    if cursor_pos == line.chars().count() {
                                        suggestion = autosuggestion(&line, &self.executor.history());
                                    } else {
                                        suggestion.clear();
                                    }
                                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                                }
                            }
                            _ => {}
                        }
                    } else if b1[0] == b'O' {
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
                                    cursor_pos = line.chars().count();
                                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
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
                                    cursor_pos = line.chars().count();
                                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                                } else if history_index < history.len() {
                                    history_index = history.len();
                                    history_line.clear();
                                    line.clear();
                                    cursor_pos = 0;
                                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                                }
                            }
                            b'C' => {
                                if cursor_pos < line.chars().count() {
                                    cursor_pos += 1;
                                    if cursor_pos == line.chars().count() {
                                        suggestion = autosuggestion(&line, &self.executor.history());
                                    } else {
                                        suggestion.clear();
                                    }
                                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                                } else if !suggestion.is_empty() {
                                    line.push_str(&suggestion);
                                    cursor_pos = line.chars().count();
                                    suggestion.clear();
                                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                                }
                            }
                            b'D' => {
                                if cursor_pos > 0 {
                                    cursor_pos -= 1;
                                    suggestion.clear();
                                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                                }
                            }
                            b'H' => {
                                cursor_pos = 0;
                                suggestion.clear();
                                redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                            }
                            b'F' => {
                                cursor_pos = line.chars().count();
                                suggestion = autosuggestion(&line, &self.executor.history());
                                redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                            }
                            _ => {}
                        }
                    } else if b1[0] == b'b' {
                        cursor_pos = prev_word_boundary(&line, cursor_pos);
                        suggestion.clear();
                        redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                    } else if b1[0] == b'f' {
                        cursor_pos = next_word_boundary(&line, cursor_pos);
                        if cursor_pos == line.chars().count() {
                            suggestion = autosuggestion(&line, &self.executor.history());
                        } else {
                            suggestion.clear();
                        }
                        redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                    } else if b1[0] == b'd' {
                        let next_pos = next_word_boundary(&line, cursor_pos);
                        if next_pos > cursor_pos {
                            let start_byte = char_to_byte_index(&line, cursor_pos);
                            let end_byte = char_to_byte_index(&line, next_pos);
                            line.drain(start_byte..end_byte);
                            if cursor_pos == line.chars().count() {
                                suggestion = autosuggestion(&line, &self.executor.history());
                            } else {
                                suggestion.clear();
                            }
                            redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                        }
                    } else if (b1[0] == 0x7f || b1[0] == 0x08)
                        && cursor_pos > 0 {
                            let prev_pos = prev_word_boundary(&line, cursor_pos);
                            let start_byte = char_to_byte_index(&line, prev_pos);
                            let end_byte = char_to_byte_index(&line, cursor_pos);
                            line.drain(start_byte..end_byte);
                            cursor_pos = prev_pos;
                            if cursor_pos == line.chars().count() {
                                suggestion = autosuggestion(&line, &self.executor.history());
                            } else {
                                suggestion.clear();
                            }
                            redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                        }
                }
                character => {
                    if character.is_control() && character != '\t' {
                        continue;
                    }
                    insert_char_at(&mut line, cursor_pos, character);
                    cursor_pos += 1;
                    if character == ' ' {
                        let old_len = line.chars().count();
                        line = expand_abbreviations_in_line(&line, &self.executor.abbreviations());
                        let new_len = line.chars().count();
                        if new_len > old_len {
                            cursor_pos += new_len - old_len;
                        }
                    }
                    if cursor_pos == line.chars().count() {
                        suggestion = autosuggestion(&line, &self.executor.history());
                    } else {
                        suggestion.clear();
                    }
                    redraw_line_with_cursor(&line, &suggestion, cursor_pos);
                }
            }
        }
    }
}

pub fn char_to_byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(s.len())
}

pub fn insert_char_at(s: &mut String, char_idx: usize, ch: char) {
    let byte_idx = char_to_byte_index(s, char_idx);
    s.insert(byte_idx, ch);
}

pub fn insert_str_at(s: &mut String, char_idx: usize, text: &str) {
    let byte_idx = char_to_byte_index(s, char_idx);
    s.insert_str(byte_idx, text);
}

pub fn remove_char_at(s: &mut String, char_idx: usize) -> Option<char> {
    if char_idx >= s.chars().count() {
        return None;
    }
    let byte_idx = char_to_byte_index(s, char_idx);
    Some(s.remove(byte_idx))
}

pub fn prev_word_boundary(s: &str, char_idx: usize) -> usize {
    if char_idx == 0 {
        return 0;
    }
    let chars: Vec<char> = s.chars().collect();
    let mut i = char_idx.min(chars.len());
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    while i > 0 && !chars[i - 1].is_whitespace() {
        i -= 1;
    }
    i
}

pub fn next_word_boundary(s: &str, char_idx: usize) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = char_idx.min(len);
    while i < len && !chars[i].is_whitespace() {
        i += 1;
    }
    while i < len && chars[i].is_whitespace() {
        i += 1;
    }
    i
}

fn redraw_line(line: &str) {
    redraw_line_with_cursor(line, "", line.chars().count());
}

fn redraw_line_with_cursor(line: &str, suggestion: &str, cursor_pos: usize) {
    let p = prompt();
    let hl = highlight_line(line);
    let line_char_count = line.chars().count();

    let effective_suggestion = if cursor_pos == line_char_count {
        suggestion
    } else {
        ""
    };

    if effective_suggestion.is_empty() {
        print!("\r\x1b[2K{}{}", p, hl);
    } else {
        print!("\r\x1b[2K{}{}\x1b[2;37m{}\x1b[0m", p, hl, effective_suggestion);
    }

    let total_rendered = line_char_count + effective_suggestion.chars().count();
    if cursor_pos < total_rendered {
        let back = total_rendered - cursor_pos;
        print!("\x1b[{}D", back);
    }
    io::stdout().flush().ok();
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


static PROMPT_PREFIX: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

pub fn set_prompt_prefix(prefix: Option<String>) {
    if let Ok(mut lock) = PROMPT_PREFIX.write() {
        *lock = prefix;
    }
}

pub fn get_prompt_prefix() -> Option<String> {
    PROMPT_PREFIX.read().ok().and_then(|l| l.clone())
}

fn prompt() -> String {
    let current = env::current_dir().unwrap_or_else(|_| PathBuf::from("?"));
    let cwd_str = current.to_string_lossy();

    let display = if let Some(idx) = cwd_str.find("/shellpilot_sandbox_").or_else(|| cwd_str.find("/mshell_sandbox_")) {
        if let Some(ws_idx) = cwd_str[idx..].find("/workspace") {
            let rel = &cwd_str[idx + ws_idx + "/workspace".len()..];
            if rel.is_empty() {
                "~/lab".to_string()
            } else {
                format!("~/lab{}", rel)
            }
        } else {
            "~/lab".to_string()
        }
    } else {
        env::var_os("HOME")
            .map(PathBuf::from)
            .and_then(|home| current.strip_prefix(home).ok().map(Path::to_path_buf))
            .map(|relative| {
                if relative.as_os_str().is_empty() {
                    "~".to_string()
                } else {
                    format!("~/{}", relative.display())
                }
            })
            .unwrap_or_else(|| current.display().to_string())
    };

    let git_context = if cwd_str.contains("/shellpilot_sandbox_") || cwd_str.contains("/mshell_sandbox_") {
        String::new()
    } else {
        git_prompt_context(&current)
    };

    let tag = if let Some(prefix) = get_prompt_prefix() {
        format!("\x1b[1;33m[{prefix}]\x1b[0m")
    } else {
        "\x1b[1;35mshellpilot\x1b[0m".to_string()
    };
    format!("{tag} \x1b[1;34m{display}\x1b[0m{git_context} $ ")
}

fn git_prompt_context(directory: &Path) -> String {
    let mut dir = directory;
    let mut git_dir: Option<PathBuf> = None;

    loop {
        let candidate = dir.join(".git");
        if candidate.is_dir() {
            git_dir = Some(candidate);
            break;
        } else if candidate.is_file()
            && let Ok(content) = fs::read_to_string(&candidate)
                && let Some(target) = content.trim().strip_prefix("gitdir:") {
                    let target_path = target.trim();
                    let p = if Path::new(target_path).is_absolute() {
                        PathBuf::from(target_path)
                    } else {
                        dir.join(target_path)
                    };
                    git_dir = Some(p);
                    break;
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
            for next in chars.by_ref() {
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

fn print_cli_help() {
    println!("ShellPilot {}", env!("CARGO_PKG_VERSION"));
    println!("The Flight Simulator for the Unix Command Line — Gamified Linux Training & Interactive Cockpit\n");
    println!("USAGE:");
    println!("    shellpilot [OPTIONS] [SCRIPT] [ARGS...]");
    println!("    pilot [OPTIONS] [SCRIPT] [ARGS...]\n");
    println!("OPTIONS:");
    println!("    -h, --help        Print help information and exit");
    println!("    -v, --version     Print version information and exit");
    println!("    -c <COMMAND>      Execute command string non-interactively");
    println!("    --tutor           Launch directly into interactive Flight Academy");
    println!("    --sandbox         Launch in isolated virtual staging server (~/lab)");
    println!("    --plain           Launch standard interactive shell without academy banner\n");
    println!("BUILTINS & COCKPIT:");
    println!("    tutor             26 progressive Flight Academy lessons across 7 curriculum tracks");
    println!("    drill             7 simulated P1/P2 production outage chaos drills");
    println!("    cadet / profile   View pilot rank, XP progression, and 11 achievement badges");
    println!("    whatif <cmd>      Dry-run blast radius analysis before running destructive commands");
    println!("    undo [diff]       Roll back workspace to pre-command state with unified diff");
    println!("    service           Manage simulated server daemons (web, worker)");
    println!("    db                Embedded SQLite query engine and schema inspector");
    println!("    tree / cheat      Visual folder hierarchy and instant offline flag cheatsheets");
    println!("    doctor            Contextual diagnostic engine for failed commands");
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
    let chars = line.chars().peekable();
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

    for c in chars {
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
                if let Some(current) = match_idx
                    && current > 0 {
                        if let Some(prev) = find_match(&query, history, current) {
                            match_idx = Some(prev);
                        } else {
                            match_idx = find_match(&query, history, history.len());
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
            "shellpilot".to_string(),
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
        let script_path = dir.join("shellpilot_test_script.sh");
        std::fs::write(&script_path, "echo foo\ntrue\n").unwrap();

        let mut shell = Shell::ephemeral();
        let args = vec![
            "shellpilot".to_string(),
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

    #[test]
    fn test_char_and_byte_indexing() {
        let ascii = "hello";
        assert_eq!(char_to_byte_index(ascii, 0), 0);
        assert_eq!(char_to_byte_index(ascii, 2), 2);
        assert_eq!(char_to_byte_index(ascii, 5), 5);
        assert_eq!(char_to_byte_index(ascii, 10), 5);

        let unicode = "привет";
        assert_eq!(char_to_byte_index(unicode, 0), 0);
        assert_eq!(char_to_byte_index(unicode, 1), 2);
        assert_eq!(char_to_byte_index(unicode, 6), 12);
    }

    #[test]
    fn test_insert_char_and_str_at() {
        let mut s = "hllo".to_string();
        insert_char_at(&mut s, 1, 'e');
        assert_eq!(s, "hello");

        insert_str_at(&mut s, 5, " world");
        assert_eq!(s, "hello world");

        let mut u = "пвет".to_string();
        insert_str_at(&mut u, 1, "ри");
        assert_eq!(u, "привет");
    }

    #[test]
    fn test_remove_char_at() {
        let mut s = "hello".to_string();
        let removed = remove_char_at(&mut s, 1);
        assert_eq!(removed, Some('e'));
        assert_eq!(s, "hllo");

        let mut u = "привет".to_string();
        let removed_u = remove_char_at(&mut u, 1);
        assert_eq!(removed_u, Some('р'));
        assert_eq!(u, "пивет");
    }

    #[test]
    fn test_word_boundary_navigation() {
        let text = "cargo test --bin shellpilot";
        // From end of string (index 27)
        let p1 = prev_word_boundary(text, 27);
        assert_eq!(p1, 17); // start of "shellpilot"
        let p2 = prev_word_boundary(text, p1);
        assert_eq!(p2, 11); // start of "--bin"
        let p3 = prev_word_boundary(text, p2);
        assert_eq!(p3, 6);  // start of "test"
        let p4 = prev_word_boundary(text, p3);
        assert_eq!(p4, 0);  // start of "cargo"

        // Next word navigation
        let n1 = next_word_boundary(text, 0);
        assert_eq!(n1, 6);  // start of "test"
        let n2 = next_word_boundary(text, n1);
        assert_eq!(n2, 11); // start of "--bin"
        let n3 = next_word_boundary(text, n2);
        assert_eq!(n3, 17); // start of "shellpilot"
        let n4 = next_word_boundary(text, n3);
        assert_eq!(n4, 27); // end of text
    }

    #[test]
    fn test_cli_help_and_version_options() {
        let mut shell = Shell::ephemeral();
        assert_eq!(shell.run_with_args(&["shellpilot".to_string(), "--help".to_string()]), 0);
        assert_eq!(shell.run_with_args(&["shellpilot".to_string(), "-h".to_string()]), 0);
        assert_eq!(shell.run_with_args(&["shellpilot".to_string(), "--version".to_string()]), 0);
        assert_eq!(shell.run_with_args(&["shellpilot".to_string(), "-v".to_string()]), 0);
        assert_eq!(shell.run_with_args(&["shellpilot".to_string(), "-V".to_string()]), 0);
    }
}
