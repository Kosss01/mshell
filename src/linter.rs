use crate::parser::{Ast, ParsedCommand, ParsedPipeline};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LintSeverity {
    Advice,
    Warning,
    Security,
}

impl std::fmt::Display for LintSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Advice => write!(f, "💡 [Unix Instructor Tip]"),
            Self::Warning => write!(f, "⚠️  [Unix Instructor Warning]"),
            Self::Security => write!(f, "🛡️  [Security Alert]"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintWarning {
    pub severity: LintSeverity,
    pub title: String,
    pub message: String,
    pub suggestion: Option<String>,
}

impl LintWarning {
    pub fn format(&self) -> String {
        let mut out = format!("{} {}\n   {}", self.severity, self.title, self.message);
        if let Some(ref sug) = self.suggestion {
            out.push_str(&format!("\n   Suggested: {}", sug));
        }
        out
    }
}

/// Analyze an AST node and return pedagogical warnings and best practice suggestions.
pub fn check_ast(ast: &Ast) -> Vec<LintWarning> {
    let mut warnings = Vec::new();
    walk_ast(ast, &mut warnings);
    warnings
}

fn walk_ast(ast: &Ast, warnings: &mut Vec<LintWarning>) {
    match ast {
        Ast::Pipeline(pipeline) => {
            check_pipeline(pipeline, warnings);
        }
        Ast::Sequence(sequence) => {
            for item in &sequence.items {
                walk_ast(&item.node, warnings);
            }
        }
        Ast::If(if_stmt) => {
            check_pipeline(&if_stmt.condition, warnings);
            for node in &if_stmt.then_branch {
                walk_ast(node, warnings);
            }
            if let Some(ref else_nodes) = if_stmt.else_branch {
                for node in else_nodes {
                    walk_ast(node, warnings);
                }
            }
        }
        Ast::For(for_loop) => {
            if let Ok(body) = for_loop.parse_body(0) {
                for node in &body {
                    walk_ast(node, warnings);
                }
            }
        }
        Ast::While(while_loop) => {
            if let Ok(condition) = while_loop.parse_condition(0) {
                for node in &condition {
                    walk_ast(node, warnings);
                }
            }
            if let Ok(body) = while_loop.parse_body(0) {
                for node in &body {
                    walk_ast(node, warnings);
                }
            }
        }
        Ast::Function(func) => {
            if let Ok(body) = func.parse_body(0) {
                for node in &body {
                    walk_ast(node, warnings);
                }
            }
        }
        Ast::StructuredPipeline(sp) => {
            walk_ast(&sp.source, warnings);
        }
    }
}

fn check_pipeline(pipeline: &ParsedPipeline, warnings: &mut Vec<LintWarning>) {
    // Check individual commands first
    for cmd in &pipeline.commands {
        check_command(cmd, warnings);
    }

    // Check pipeline-level anti-patterns
    if pipeline.commands.len() >= 2 {
        let first = &pipeline.commands[0];
        let second = &pipeline.commands[1];

        // 1. Useless Use of Cat (UUOC)
        if first.program == "cat" {
            let files: Vec<&String> = first.args.iter().filter(|a| !a.starts_with('-')).collect();
            if !files.is_empty() {
                let filename = files[0];
                match second.program.as_str() {
                    "grep" => {
                        let grep_args = second.args.join(" ");
                        let suggestion = if grep_args.is_empty() {
                            format!("grep <pattern> {}", filename)
                        } else {
                            format!("grep {} {}", grep_args, filename)
                        };
                        warnings.push(LintWarning {
                            severity: LintSeverity::Advice,
                            title: "Useless Use of Cat (UUOC)".into(),
                            message: format!(
                                "'cat {filename} | grep ...' spawns an unnecessary cat process. 'grep' can read files directly."
                            ),
                            suggestion: Some(suggestion),
                        });
                    }
                    "wc" => {
                        let wc_args = second.args.join(" ");
                        let args_str = if wc_args.is_empty() {
                            String::new()
                        } else {
                            format!("{} ", wc_args)
                        };
                        warnings.push(LintWarning {
                            severity: LintSeverity::Advice,
                            title: "Useless Use of Cat (UUOC)".into(),
                            message: format!(
                                "'cat {filename} | wc' is redundant. 'wc' can inspect the file or take input redirection."
                            ),
                            suggestion: Some(format!("wc {}{}", args_str, filename)),
                        });
                    }
                    "sed" => {
                        let sed_args = second.args.join(" ");
                        warnings.push(LintWarning {
                            severity: LintSeverity::Advice,
                            title: "Useless Use of Cat (UUOC)".into(),
                            message: format!(
                                "'cat {filename} | sed ...' is redundant. 'sed' accepts filename arguments directly."
                            ),
                            suggestion: Some(format!("sed {} {}", sed_args, filename)),
                        });
                    }
                    "awk" => {
                        let awk_args = second.args.join(" ");
                        warnings.push(LintWarning {
                            severity: LintSeverity::Advice,
                            title: "Useless Use of Cat (UUOC)".into(),
                            message: format!(
                                "'cat {filename} | awk ...' is redundant. 'awk' accepts filename arguments directly."
                            ),
                            suggestion: Some(format!("awk {} {}", awk_args, filename)),
                        });
                    }
                    _ => {}
                }
            }
        }

        // 2. Useless Use of Echo to grep
        if first.program == "echo" && second.program == "grep" {
            let echo_text = first.args.join(" ");
            let grep_args = second.args.join(" ");
            warnings.push(LintWarning {
                severity: LintSeverity::Advice,
                title: "Unnecessary pipe from echo".into(),
                message: "Piping echo into grep creates an extra subshell. In modern shells you can use here-strings."
                    .into(),
                suggestion: Some(format!("grep {} <<< \"{}\"", grep_args, echo_text)),
            });
        }
    }
}

fn check_command(cmd: &ParsedCommand, warnings: &mut Vec<LintWarning>) {
    match cmd.program.as_str() {
        // 3. Insecure Permissions (chmod 777)
        "chmod" => {
            let has_777 = cmd.args.iter().any(|arg| {
                arg == "777"
                    || arg == "0777"
                    || arg == "a+rwx"
                    || arg == "ugo+rwx"
                    || arg == "+rwx"
            });
            if has_777 {
                warnings.push(LintWarning {
                    severity: LintSeverity::Security,
                    title: "Insecure Permission Mode (777)".into(),
                    message: "Setting mode 777 (rwxrwxrwx) makes files world-writable, allowing any local user or malicious process to overwrite or compromise them.".into(),
                    suggestion: Some("Use 'chmod 755' (rwxr-xr-x) for executables/directories or 'chmod 644' (rw-r--r--) for files.".into()),
                });
            }
        }

        // 4. Dangerous kill -9
        "kill" => {
            let has_force = cmd.args.iter().any(|arg| {
                arg == "-9"
                    || arg == "-KILL"
                    || arg == "-SIGKILL"
                    || (arg == "-s" && cmd.args.iter().any(|a| a == "9" || a == "KILL"))
            });
            if has_force {
                warnings.push(LintWarning {
                    severity: LintSeverity::Warning,
                    title: "Immediate Process Termination (SIGKILL / kill -9)".into(),
                    message: "SIGKILL immediately kills the process without allowing it to clean up temporary files, release database locks, or finish pending I/O.".into(),
                    suggestion: Some("Attempt graceful shutdown with 'kill -15' (SIGTERM) or default 'kill <pid>' before using -9.".into()),
                });
            }
        }

        // 5. Nested mkdir without -p
        "mkdir" => {
            let has_p = cmd.args.iter().any(|arg| {
                arg == "-p"
                    || arg == "--parents"
                    || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('p'))
            });
            if !has_p {
                let nested_target = cmd
                    .args
                    .iter()
                    .filter(|arg| !arg.starts_with('-'))
                    .find(|path| path.contains('/'));

                if let Some(target) = nested_target {
                    warnings.push(LintWarning {
                        severity: LintSeverity::Advice,
                        title: "Nested Directory Creation Without -p".into(),
                        message: format!(
                            "Path '{target}' contains nested directories. 'mkdir' will fail if parent directories do not already exist."
                        ),
                        suggestion: Some(format!("mkdir -p {}", target)),
                    });
                }
            }
        }

        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_ast;

    #[test]
    fn catches_useless_use_of_cat_with_grep() {
        let ast = parse_ast("cat file.txt | grep hello", 0).unwrap().unwrap();
        let warnings = check_ast(&ast);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].severity, LintSeverity::Advice);
        assert!(warnings[0].title.contains("Useless Use of Cat"));
        assert!(warnings[0].suggestion.as_ref().unwrap().contains("grep hello file.txt"));
    }

    #[test]
    fn catches_chmod_777_security_risk() {
        let ast = parse_ast("chmod 777 secret.sh", 0).unwrap().unwrap();
        let warnings = check_ast(&ast);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].severity, LintSeverity::Security);
        assert!(warnings[0].message.contains("world-writable"));
    }

    #[test]
    fn catches_kill_9_warning() {
        let ast = parse_ast("kill -9 1234", 0).unwrap().unwrap();
        let warnings = check_ast(&ast);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].severity, LintSeverity::Warning);
        assert!(warnings[0].suggestion.as_ref().unwrap().contains("SIGTERM"));
    }

    #[test]
    fn catches_nested_mkdir_without_p() {
        let ast = parse_ast("mkdir a/b/c", 0).unwrap().unwrap();
        let warnings = check_ast(&ast);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].suggestion.as_ref().unwrap().contains("mkdir -p a/b/c"));
    }

    #[test]
    fn catches_echo_pipe_warning() {
        let ast = parse_ast("echo hello | grep hello", 0).unwrap().unwrap();
        let warnings = check_ast(&ast);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].title.contains("Unnecessary pipe from echo"));
    }

    #[test]
    fn catches_cat_wc_warning() {
        let ast = parse_ast("cat data.log | wc -l", 0).unwrap().unwrap();
        let warnings = check_ast(&ast);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].title.contains("Useless Use of Cat"));
        assert!(warnings[0].suggestion.as_ref().unwrap().contains("wc -l data.log"));
    }

    #[test]
    fn allows_proper_idiomatic_commands() {
        let ast = parse_ast("grep hello file.txt && chmod 755 run.sh && mkdir -p a/b/c", 0)
            .unwrap()
            .unwrap();
        let warnings = check_ast(&ast);
        assert!(warnings.is_empty());
    }
}
