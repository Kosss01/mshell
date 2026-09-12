use std::path::Path;
use std::process::Command;

use crate::completions::HelpCompletionExtractor;
use crate::effects::{self, BlastRadius, Effect};
use crate::executor::find_executable_in_path;
use crate::parser::{
    Ast, AstSequence, FilterOp, OutputFormat, ParsedCommand, ParsedPipeline, ParsedStructuredPipeline,
    Redirection, StructuredOp,
};
use crate::policy::{self, PolicyDecision, RiskLevel};

/// Comprehensive explanation of an AST command or pipeline.
pub fn explain_ast(ast: &Ast, extractor: Option<&HelpCompletionExtractor>) -> String {
    let mut out = String::new();

    // 1. Structure & Command Breakdown
    out.push_str("Command Breakdown:\n");
    explain_ast_nodes(ast, extractor, &mut out, 1);

    // 2. Effects Analysis
    let effects = effects::analyze_ast(ast);
    out.push_str("\nEffects Identified:\n");
    if effects.is_empty() {
        out.push_str("  • None (pure shell evaluation)\n");
    } else {
        for effect in &effects {
            out.push_str(&format!("  • {}\n", format_effect(effect)));
        }
    }

    // 3. Blast Radius Preview (if applicable)
    let blast_radii = find_blast_radii(&effects);
    if !blast_radii.is_empty() {
        out.push_str("\nBlast Radius Preview:\n");
        for (path, radius) in blast_radii {
            let sensitive = if effects::is_sensitive_path(&path) {
                " 🚨 [SENSITIVE ROOT PATH]"
            } else {
                ""
            };
            out.push_str(&format!(
                "  ⚠️  Will affect {} file(s) ({}) in '{}'{}\n",
                radius.file_count,
                effects::format_bytes(radius.total_bytes),
                path,
                sensitive
            ));
        }
    }

    for effect in &effects {
        if let Effect::UntrustedPipelineExecution(interp) = effect {
            out.push_str(&format!(
                "\n⚠️  CRITICAL DATA-FLOW WARNING:\n  Remote network input is directly piped into interpreter '{interp}'.\n  This executes remote scripts without prior local inspection.\n"
            ));
        }
    }

    // 4. Security Policy
    let decision = policy::evaluate(&effects);
    let risk = policy::risk_level(&effects);
    out.push_str("\nSecurity Policy:\n");
    out.push_str(&format!("  • Decision: {}\n", format_decision(decision)));
    out.push_str(&format!("  • Risk Level: {:?}\n", risk));
    out.push_str(&format!("  • Note: {}\n", decision_note(decision, risk)));

    out
}

fn format_decision(decision: PolicyDecision) -> &'static str {
    match decision {
        PolicyDecision::Allow => "ALLOW (safe to run directly)",
        PolicyDecision::Ask => "ASK (requires confirmation in interactive mode)",
        PolicyDecision::Block => "BLOCK (prohibited by safety guardrails)",
    }
}

fn decision_note(decision: PolicyDecision, risk: RiskLevel) -> &'static str {
    match decision {
        PolicyDecision::Block => "Command targets critical system locations or privileged actions.",
        PolicyDecision::Ask => "Command modifies files or executes destructive actions with non-zero blast radius.",
        PolicyDecision::Allow => match risk {
            RiskLevel::Low => "Standard execution with low risk footprint.",
            _ => "Allowed under standard shell policy.",
        },
    }
}

fn format_effect(effect: &Effect) -> String {
    match effect {
        Effect::FilesystemRead(p) => format!("FilesystemRead: Reads from file or directory '{p}'"),
        Effect::FilesystemWrite(p) => format!("FilesystemWrite: Writes or overwrites file '{p}'"),
        Effect::FilesystemDelete(p) => format!("FilesystemDelete: Deletes files or directories at '{p}'"),
        Effect::SensitivePathRead(p) => format!("SensitivePathRead: Reads sensitive system file at '{p}'"),
        Effect::SensitivePathWrite(p) => format!("SensitivePathWrite: Modifies critical system file or path at '{p}'"),
        Effect::NetworkAccess => "NetworkAccess: Establishes external network communication".to_string(),
        Effect::PrivilegeChange => "PrivilegeChange: Changes user/group privileges or executes with root access".to_string(),
        Effect::ProcessCreation => "ProcessCreation: Spawns an isolated child process".to_string(),
        Effect::PipelineDataFlow => "PipelineDataFlow: Streams data between processes via UNIX pipe".to_string(),
        Effect::UntrustedPipelineExecution(interp) => {
            format!("UntrustedPipelineExecution: Streams untrusted network input directly into interpreter '{interp}'")
        }
        Effect::PackageInstallation(pkg) => {
            format!("PackageInstallation: Installs or modifies system packages using '{pkg}'")
        }
        Effect::ProcessKill(target) => format!("ProcessKill: Terminates process '{target}'"),
    }
}

fn find_blast_radii(effects: &[Effect]) -> Vec<(String, BlastRadius)> {
    let mut list = Vec::new();
    for effect in effects {
        let path = match effect {
            Effect::FilesystemDelete(p)
            | Effect::SensitivePathWrite(p)
            | Effect::FilesystemWrite(p) => p,
            _ => continue,
        };
        if let Some(radius) = effects::calculate_blast_radius_for_path(path) {
            list.push((path.clone(), radius));
        }
    }
    list
}

fn explain_ast_nodes(ast: &Ast, extractor: Option<&HelpCompletionExtractor>, out: &mut String, _stage: usize) {
    match ast {
        Ast::Pipeline(pipeline) => explain_pipeline(pipeline, extractor, out),
        Ast::StructuredPipeline(sp) => explain_structured_pipeline(sp, extractor, out),
        Ast::Sequence(seq) => explain_sequence(seq, extractor, out),
        Ast::If(if_stmt) => {
            out.push_str("  • Condition:\n");
            explain_pipeline(&if_stmt.condition, extractor, out);
            out.push_str("  • Then Branch:\n");
            for (idx, then_node) in if_stmt.then_branch.iter().enumerate() {
                explain_ast_nodes(then_node, extractor, out, idx + 1);
            }
            if let Some(else_branch) = &if_stmt.else_branch {
                out.push_str("  • Else Branch:\n");
                for (idx, else_node) in else_branch.iter().enumerate() {
                    explain_ast_nodes(else_node, extractor, out, idx + 1);
                }
            }
        }
        Ast::For(parsed_for) => {
            out.push_str(&format!("  • For Loop (variable: {})\n", parsed_for.variable));
            if let Ok(body) = parsed_for.parse_body(0) {
                out.push_str("  • Body:\n");
                for (idx, body_node) in body.iter().enumerate() {
                    explain_ast_nodes(body_node, extractor, out, idx + 1);
                }
            }
        }
        Ast::While(parsed_while) => {
            out.push_str("  • While Loop Condition:\n");
            if let Ok(cond) = parsed_while.parse_condition(0) {
                for (idx, cond_node) in cond.iter().enumerate() {
                    explain_ast_nodes(cond_node, extractor, out, idx + 1);
                }
            }
            if let Ok(body) = parsed_while.parse_body(0) {
                out.push_str("  • Body:\n");
                for (idx, body_node) in body.iter().enumerate() {
                    explain_ast_nodes(body_node, extractor, out, idx + 1);
                }
            }
        }
        Ast::Function(func) => {
            out.push_str(&format!("  • Shell Function Definition: '{}'\n", func.name));
            if let Ok(body) = func.parse_body(0) {
                out.push_str("  • Body:\n");
                for (idx, body_node) in body.iter().enumerate() {
                    explain_ast_nodes(body_node, extractor, out, idx + 1);
                }
            }
        }
    }
}

fn explain_pipeline(pipeline: &ParsedPipeline, extractor: Option<&HelpCompletionExtractor>, out: &mut String) {
    if pipeline.commands.len() == 1 {
        explain_single_command(&pipeline.commands[0], extractor, out, None);
    } else {
        for (i, cmd) in pipeline.commands.iter().enumerate() {
            explain_single_command(cmd, extractor, out, Some(i + 1));
            if i + 1 < pipeline.commands.len() {
                out.push_str("    │ Pipe (|): Streams standard output to next command\n");
            }
        }
    }
}

fn explain_structured_pipeline(
    sp: &ParsedStructuredPipeline,
    extractor: Option<&HelpCompletionExtractor>,
    out: &mut String,
) {
    out.push_str("  • Source Stage:\n");
    explain_ast_nodes(&sp.source, extractor, out, 1);

    out.push_str("  • Structured Transformations (|>):\n");
    for (i, stage) in sp.stages.iter().enumerate() {
        match stage {
            StructuredOp::Project(field) => {
                out.push_str(&format!("    {}. Projection: Extracts field '{}' from records\n", i + 1, field));
            }
            StructuredOp::Filter(expr) => {
                let op_sym = match expr.op {
                    FilterOp::Equal => "==",
                    FilterOp::NotEqual => "!=",
                    FilterOp::GreaterThan => ">",
                    FilterOp::LessThan => "<",
                    FilterOp::GreaterOrEqual => ">=",
                    FilterOp::LessOrEqual => "<=",
                    FilterOp::Exists => "exists",
                };
                out.push_str(&format!(
                    "    {}. Filter: Selects records where '{}' {} \"{}\"\n",
                    i + 1, expr.field, op_sym, expr.value
                ));
            }
            StructuredOp::Take(n) => {
                out.push_str(&format!("    {}. Limit: Takes first {} records\n", i + 1, n));
            }
            StructuredOp::Skip(n) => {
                out.push_str(&format!("    {}. Skip: Skips first {} records\n", i + 1, n));
            }
            StructuredOp::Count => {
                out.push_str(&format!("    {}. Aggregate: Counts total records\n", i + 1));
            }
            StructuredOp::Format(fmt) => {
                let name = match fmt {
                    OutputFormat::Json => "JSON",
                    OutputFormat::Yaml => "YAML",
                    OutputFormat::Toml => "TOML",
                    OutputFormat::Table => "Table",
                };
                out.push_str(&format!("    {}. Format: Converts output into {}\n", i + 1, name));
            }
        }
    }
}

fn explain_sequence(seq: &AstSequence, extractor: Option<&HelpCompletionExtractor>, out: &mut String) {
    for (i, item) in seq.items.iter().enumerate() {
        if let Some(op) = &item.op {
            let op_name = match op {
                crate::parser::LogicalOp::And => "Logical AND (&&): Run next command only if previous succeeded (exit 0)",
                crate::parser::LogicalOp::Or => "Logical OR (||): Run next command only if previous failed",
            };
            out.push_str(&format!("  [Operator: {op_name}]\n"));
        } else if i > 0 {
            out.push_str("  [Sequence: Semicolon (;) / Newline]\n");
        }
        explain_ast_nodes(&item.node, extractor, out, i + 1);
    }
}

fn explain_single_command(
    cmd: &ParsedCommand,
    extractor: Option<&HelpCompletionExtractor>,
    out: &mut String,
    stage_num: Option<usize>,
) {
    let prefix = if let Some(n) = stage_num {
        format!("  • Stage {n}: ")
    } else {
        "  • Command: ".to_string()
    };

    let mut full_cmd = cmd.program.clone();
    if !cmd.args.is_empty() {
        full_cmd.push(' ');
        full_cmd.push_str(&cmd.args.join(" "));
    }

    out.push_str(&format!("{prefix}{full_cmd}\n"));

    let is_builtin = crate::builtins::is_builtin(&cmd.program);
    let resolved_path = find_executable_in_path(&cmd.program);

    let type_desc = match (is_builtin, &resolved_path) {
        (true, Some(path)) => format!("Shell Builtin (also in PATH at {})", path.display()),
        (true, None) => "Shell Builtin (executes internally without external process)".to_string(),
        (false, Some(path)) => format!("External Executable ({})", path.display()),
        (false, None) => "External Command (not found in current PATH)".to_string(),
    };
    out.push_str(&format!("    - Type: {type_desc}\n"));

    let description = lookup_command_description(&cmd.program);
    out.push_str(&format!("    - Purpose: {description}\n"));

    // Flag explanations
    let (flags, positional) = partition_flags_and_args(&cmd.args);
    if !flags.is_empty() {
        out.push_str("    - Flags & Options:\n");
        for flag in &flags {
            let flag_desc = lookup_flag_description(&cmd.program, flag, extractor);
            out.push_str(&format!("      • {flag}: {flag_desc}\n"));
        }
    }

    if !positional.is_empty() {
        out.push_str(&format!("    - Arguments: {}\n", positional.join(", ")));
    }

    if !cmd.environment.is_empty() {
        out.push_str("    - Environment Variables:\n");
        for (k, v) in &cmd.environment {
            out.push_str(&format!("      • {k} = \"{v}\"\n"));
        }
    }

    if !cmd.redirects.is_empty() {
        out.push_str("    - I/O Redirections:\n");
        for redir in &cmd.redirects {
            out.push_str(&format!("      • {}\n", format_redirection(redir)));
        }
    }
}

fn format_redirection(redir: &Redirection) -> String {
    match redir {
        Redirection::Stdin(p) => format!("< {p} (Read stdin from file '{p}')"),
        Redirection::Stdout(p) => format!("> {p} (Overwrite '{p}' with stdout)"),
        Redirection::StdoutAppend(p) => format!(">> {p} (Append stdout to '{p}')"),
        Redirection::Stderr(p) => format!("2> {p} (Overwrite '{p}' with stderr)"),
        Redirection::StderrAppend(p) => format!("2>> {p} (Append stderr to '{p}')"),
        Redirection::StdoutAndStderr(p) => format!("&> {p} (Overwrite '{p}' with stdout and stderr)"),
        Redirection::StdoutAndStderrAppend(p) => format!("&>> {p} (Append stdout and stderr to '{p}')"),
        Redirection::DupWrite(from, to) => format!("{from}>&{to} (Redirect file descriptor {from} to {to})"),
        Redirection::DupRead(from, to) => format!("{from}<&{to} (Duplicate file descriptor {from} from {to})"),
        Redirection::HereString(s) => format!("<<< \"{s}\" (Pass inline string as stdin)"),
    }
}

fn partition_flags_and_args(args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut flags = Vec::new();
    let mut pos = Vec::new();
    let mut parsing_flags = true;

    for arg in args {
        if !parsing_flags {
            pos.push(arg.clone());
        } else if arg == "--" {
            parsing_flags = false;
        } else if arg.starts_with("--") && arg.len() > 2 {
            flags.push(arg.clone());
        } else if arg.starts_with('-') && arg.len() > 1 && !arg.chars().skip(1).all(|c| c.is_ascii_digit()) {
            // Check if combined short flags like -rf or -la
            if arg.len() > 2 && !arg.contains('=') {
                for c in arg.chars().skip(1) {
                    flags.push(format!("-{c}"));
                }
            } else {
                flags.push(arg.clone());
            }
        } else {
            pos.push(arg.clone());
        }
    }

    (flags, pos)
}

fn lookup_command_description(name: &str) -> String {
    let base = Path::new(name).file_name().and_then(|n| n.to_str()).unwrap_or(name);

    match base {
        // Builtins
        "pwd" => "Print the full filename of the current working directory.".into(),
        "cd" => "Change the shell working directory.".into(),
        "echo" => "Write arguments to standard output.".into(),
        "printf" => "Format and print arguments to standard output.".into(),
        "exit" => "Exit the ShellPilot shell.".into(),
        "export" => "Set export attribute for shell variables.".into(),
        "unset" => "Unset values and attributes of shell variables.".into(),
        "env" => "Display environment variables or run a command in an altered environment.".into(),
        "type" => "Display information about command type (builtin, alias, or executable).".into(),
        "help" => "Display information about builtin commands.".into(),
        "history" => "Display execution history from persistent timeline.".into(),
        "abbr" => "Manage ergonomic abbreviations (auto-expanded in interactive mode).".into(),
        "explain" => "Analyze and explain command purpose, structure, effects, and risk.".into(),
        "policy" => "Evaluate safety policy decision and risk level for a command.".into(),
        "jobs" => "List active background jobs.".into(),
        "fg" => "Move job to the foreground.".into(),
        "bg" => "Move job to the background.".into(),
        "wait" => "Wait for process completion and return exit status.".into(),
        "kill" => "Send a signal to a process or job.".into(),
        "processes" => "List running processes on the system.".into(),
        "ports" => "List listening network endpoints and ports.".into(),
        "connections" => "List active TCP/UDP network connections.".into(),
        "json" => "Validate, format, or inspect JSON structured data.".into(),
        "yaml" => "Validate or format YAML structured data.".into(),
        "toml" => "Validate or format TOML structured data.".into(),

        // Common Unix utilities
        "ls" => "List information about files in directory.".into(),
        "rm" => "Remove files or directories.".into(),
        "cp" => "Copy files and directories.".into(),
        "mv" => "Move (rename) files or directories.".into(),
        "mkdir" => "Create directories if they do not already exist.".into(),
        "rmdir" => "Remove empty directories.".into(),
        "touch" => "Update the access and modification times of files, or create empty files.".into(),
        "cat" => "Concatenate files and print on the standard output.".into(),
        "grep" => "Print lines that match pattern.".into(),
        "find" => "Search for files in a directory hierarchy.".into(),
        "chmod" => "Change file mode bits (permissions).".into(),
        "chown" => "Change file owner and group.".into(),
        "curl" => "Transfer data from or to a server using supported network protocols.".into(),
        "wget" => "Non-interactive network downloader.".into(),
        "git" => "Fast, scalable, distributed revision control system.".into(),
        "docker" => "Pack, ship and run any application as a lightweight container.".into(),
        "kubectl" => "Kubernetes command-line tool for controlling clusters.".into(),
        "dd" => "Convert and copy a file or raw disk data.".into(),
        "tar" => "An archiving utility for packing files together.".into(),
        "ssh" => "OpenSSH SSH client (remote login program).".into(),
        "sudo" => "Execute a command as another user or superuser (root).".into(),
        "su" => "Change user ID or become superuser.".into(),
        "ps" => "Report a snapshot of the current processes.".into(),
        "killall" => "Kill processes by name.".into(),
        "awk" => "Pattern scanning and processing language.".into(),
        "sed" => "Stream editor for filtering and transforming text.".into(),
        "head" => "Output the first part of files.".into(),
        "tail" => "Output the last part of files.".into(),
        "sort" => "Sort lines of text files.".into(),
        "uniq" => "Report or omit repeated lines.".into(),
        "wc" => "Print newline, word, and byte counts for each file.".into(),
        "diff" => "Compare files line by line.".into(),
        "man" => "An interface to the on-line reference manuals.".into(),
        "which" => "Locate a command executable in PATH.".into(),
        "whoami" => "Print effective user ID.".into(),
        "uname" => "Print system information.".into(),
        "df" => "Report file system disk space usage.".into(),
        "du" => "Estimate file space usage.".into(),
        "free" => "Display amount of free and used memory in the system.".into(),
        "top" => "Display Linux processes in real-time.".into(),
        "htop" => "Interactive process viewer.".into(),
        "cargo" => "The Rust package manager and build system.".into(),
        "rustc" => "The Rust compiler.".into(),
        "python" | "python3" => "Interactive high-level object-oriented language interpreter.".into(),
        "node" => "JavaScript runtime built on Chrome's V8 engine.".into(),
        "npm" => "Node package manager.".into(),
        "make" => "GNU make utility to maintain groups of programs.".into(),
        "gcc" => "GNU project C and C++ compiler.".into(),
        "clang" => "C, C++, and Objective-C compiler based on LLVM.".into(),

        // Dynamic fallback: query --help first line or whatis
        _ => query_external_command_summary(name),
    }
}

fn query_external_command_summary(name: &str) -> String {
    // Try whatis command
    if let Ok(output) = Command::new("whatis").arg(name).output()
        && output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(line) = text.lines().next()
                && let Some(desc) = line.split(" - ").nth(1) {
                    return desc.trim().to_string();
                }
        }

    // Try --help first non-usage line
    if let Ok(output) = Command::new(name).arg("--help").output() {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let t = line.trim();
            if !t.is_empty()
                && !t.starts_with("Usage:")
                && !t.starts_with("usage:")
                && !t.starts_with('[')
                && !t.starts_with('-')
            {
                return t.to_string();
            }
        }
    }

    "External system command or user executable.".into()
}

fn lookup_flag_description(
    cmd: &str,
    flag: &str,
    extractor: Option<&HelpCompletionExtractor>,
) -> String {
    let clean_flag = if let Some(idx) = flag.find('=') {
        &flag[..idx]
    } else {
        flag
    };

    // 1. Common flag dictionary
    match (cmd, clean_flag) {
        // rm
        ("rm", "-r") | ("rm", "-R") | ("rm", "--recursive") => "Remove directories and their contents recursively".into(),
        ("rm", "-f") | ("rm", "--force") => "Ignore nonexistent files and arguments, never prompt".into(),
        ("rm", "-i") => "Prompt before every removal".into(),
        ("rm", "-v") | ("rm", "--verbose") => "Explain what is being done".into(),
        ("rm", "-d") | ("rm", "--dir") => "Remove empty directories".into(),

        // ls
        ("ls", "-a") | ("ls", "--all") => "Do not ignore entries starting with .".into(),
        ("ls", "-l") => "Use a long listing format".into(),
        ("ls", "-h") | ("ls", "--human-readable") => "Print human readable sizes (e.g., 1K 234M 2G)".into(),
        ("ls", "-R") | ("ls", "--recursive") => "List subdirectories recursively".into(),
        ("ls", "-t") => "Sort by time, newest first".into(),
        ("ls", "-S") => "Sort by file size, largest first".into(),

        // chmod & chown
        ("chmod", "-R") | ("chmod", "--recursive") => "Change files and directories recursively".into(),
        ("chown", "-R") | ("chown", "--recursive") => "Operate on files and directories recursively".into(),

        // grep
        ("grep", "-i") | ("grep", "--ignore-case") => "Ignore case distinctions in patterns and input".into(),
        ("grep", "-v") | ("grep", "--invert-match") => "Select non-matching lines".into(),
        ("grep", "-r") | ("grep", "-R") | ("grep", "--recursive") => "Read all files under each directory, recursively".into(),
        ("grep", "-n") | ("grep", "--line-number") => "Prefix each line of output with the 1-based line number".into(),
        ("grep", "-c") | ("grep", "--count") => "Print only a count of selected lines per FILE".into(),
        ("grep", "-l") | ("grep", "--files-with-matches") => "Print only names of FILEs with selected lines".into(),

        // curl
        ("curl", "-s") | ("curl", "--silent") => "Silent mode (do not show progress meter or error messages)".into(),
        ("curl", "-v") | ("curl", "--verbose") => "Make the operation more talkative".into(),
        ("curl", "-L") | ("curl", "--location") => "Follow HTTP redirects".into(),
        ("curl", "-o") | ("curl", "--output") => "Write output to specified file instead of stdout".into(),
        ("curl", "-X") | ("curl", "--request") => "Specify request method to use".into(),

        // dd
        ("dd", f) if f.starts_with("if=") => format!("Read from source file/device '{}'", &f[3..]),
        ("dd", f) if f.starts_with("of=") => format!("Write to destination file/device '{}'", &f[3..]),

        // Generic
        (_, "-h") | (_, "--help") => "Display help message and exit".into(),
        (_, "-v") | (_, "-V") | (_, "--version") => "Output version information and exit".into(),
        (_, "-q") | (_, "--quiet") => "Quiet or suppress non-error messages".into(),
        (_, "--verbose") => "Produce detailed log output".into(),

        // Fallback: extract from HelpCompletionExtractor
        _ => {
            if let Some(ext) = extractor {
                let completions = ext.complete_flags(cmd, clean_flag);
                if let Some(matched) = completions.into_iter().find(|c| c.flag == clean_flag)
                    && !matched.description.is_empty() {
                        return matched.description;
                    }
            }
            "Command-specific flag or option.".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explains_pwd_command_with_purpose_and_effects() {
        let ast = crate::parser::parse_ast("pwd", 0).unwrap().unwrap();
        let explanation = explain_ast(&ast, None);

        assert!(explanation.contains("Command: pwd"));
        assert!(explanation.contains("Shell Builtin"));
        assert!(explanation.contains("Print the full filename of the current working directory"));
        assert!(explanation.contains("Decision: ALLOW"));
        assert!(explanation.contains("Risk Level: Low"));
    }

    #[test]
    fn explains_rm_rf_with_flags_and_blast_radius() {
        let ast = crate::parser::parse_ast("rm -rf /tmp/melchior_test_dir", 0).unwrap().unwrap();
        let explanation = explain_ast(&ast, None);

        assert!(explanation.contains("Command: rm -rf /tmp/melchior_test_dir"));
        assert!(explanation.contains("Remove files or directories"));
        assert!(explanation.contains("-r: Remove directories and their contents recursively"));
        assert!(explanation.contains("-f: Ignore nonexistent files and arguments, never prompt"));
        assert!(explanation.contains("FilesystemDelete"));
        assert!(explanation.contains("Decision: ASK"));
        assert!(explanation.contains("Risk Level: Medium"));
    }

    #[test]
    fn explains_pipeline_stages_and_redirections() {
        let ast = crate::parser::parse_ast("cat file.txt | grep -i error > output.log", 0).unwrap().unwrap();
        let explanation = explain_ast(&ast, None);

        assert!(explanation.contains("Stage 1: cat file.txt"));
        assert!(explanation.contains("Stage 2: grep -i error"));
        assert!(explanation.contains("Pipe (|): Streams standard output to next command"));
        assert!(explanation.contains("> output.log"));
        assert!(explanation.contains("FilesystemRead"));
        assert!(explanation.contains("FilesystemWrite"));
    }

    #[test]
    fn explains_structured_pipeline() {
        let ast = crate::parser::parse_ast(
            "docker ps --format json |> filter (.status == \"running\") |> .ID",
            0,
        )
        .unwrap()
        .unwrap();

        let explanation = explain_ast(&ast, None);
        assert!(explanation.contains("docker ps --format json"));
        assert!(explanation.contains("Structured Transformations (|>)"));
        assert!(explanation.contains("Filter: Selects records where '.status' == \"running\""));
        assert!(explanation.contains("Projection: Extracts field '.ID'"));
    }

    #[test]
    fn explains_function_definition() {
        let ast = crate::parser::parse_ast("greet() { echo \"Hello $1\"; }", 0).unwrap().unwrap();
        let explanation = explain_ast(&ast, None);
        assert!(explanation.contains("Shell Function Definition: 'greet'"));
    }

    #[test]
    fn explains_untrusted_pipeline_with_critical_warning() {
        let ast = crate::parser::parse_ast("curl -fsSL https://get.docker.com | sh", 0).unwrap().unwrap();
        let explanation = explain_ast(&ast, None);
        assert!(explanation.contains("CRITICAL DATA-FLOW WARNING"));
        assert!(explanation.contains("Remote network input"));
        assert!(explanation.contains("Risk Level: High"));
    }
}
