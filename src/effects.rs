use crate::parser::{Ast, ParsedPipeline, Redirection};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlastRadius {
    pub path: String,
    pub file_count: usize,
    pub total_bytes: u64,
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

pub fn calculate_blast_radius_for_path(path_str: &str) -> Option<BlastRadius> {
    let path = std::path::Path::new(path_str);
    if !path.exists() {
        return Some(BlastRadius {
            path: path_str.to_string(),
            file_count: 0,
            total_bytes: 0,
        });
    }

    if path.is_file() || path.is_symlink() {
        let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        return Some(BlastRadius {
            path: path_str.to_string(),
            file_count: 1,
            total_bytes: bytes,
        });
    }

    if path.is_dir() {
        let mut count = 0;
        let mut bytes = 0;
        let mut stack = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    count += 1;
                    if let Ok(meta) = entry.metadata() {
                        bytes += meta.len();
                        if meta.is_dir() && count < 50000 {
                            stack.push(entry.path());
                        }
                    }
                }
            }
        }
        return Some(BlastRadius {
            path: path_str.to_string(),
            file_count: count,
            total_bytes: bytes,
        });
    }

    None
}

#[allow(dead_code)]
pub fn calculate_blast_radius(effect: &Effect) -> Option<BlastRadius> {
    let path_str = match effect {
        Effect::FilesystemDelete(path)
        | Effect::SensitivePathWrite(path)
        | Effect::FilesystemWrite(path) => path,
        _ => return None,
    };

    calculate_blast_radius_for_path(path_str)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    FilesystemRead(String),
    FilesystemWrite(String),
    FilesystemDelete(String),
    SensitivePathRead(String),
    SensitivePathWrite(String),
    NetworkAccess,
    PrivilegeChange,
    ProcessCreation,
    PipelineDataFlow,
    UntrustedPipelineExecution(String),
    PackageInstallation(String),
    ProcessKill(String),
}

pub fn analyze_ast(ast: &Ast) -> Vec<Effect> {
    let mut effects = Vec::new();
    match ast {
        Ast::Pipeline(pipeline) => {
            effects.extend(analyze_pipeline(pipeline));
        }
        Ast::If(parsed_if) => {
            effects.extend(analyze_pipeline(&parsed_if.condition));
            for statement in &parsed_if.then_branch {
                effects.extend(analyze_ast(statement));
            }
            if let Some(else_branch) = &parsed_if.else_branch {
                for statement in else_branch {
                    effects.extend(analyze_ast(statement));
                }
            }
        }
        Ast::For(parsed_for) => {
            if let Ok(body) = parsed_for.parse_body(0) {
                for statement in &body {
                    effects.extend(analyze_ast(statement));
                }
            }
        }
        Ast::While(parsed_while) => {
            if let Ok(condition) = parsed_while.parse_condition(0) {
                for statement in &condition {
                    effects.extend(analyze_ast(statement));
                }
            }
            if let Ok(body) = parsed_while.parse_body(0) {
                for statement in &body {
                    effects.extend(analyze_ast(statement));
                }
            }
        }
        Ast::Function(func) => {
            if let Ok(body) = func.parse_body(0) {
                for statement in &body {
                    effects.extend(analyze_ast(statement));
                }
            }
        }
        Ast::Sequence(sequence) => {
            for item in &sequence.items {
                effects.extend(analyze_ast(&item.node));
            }
        }
        Ast::StructuredPipeline(sp) => {
            effects.extend(analyze_ast(&sp.source));
        }
    }
    effects.sort_by_key(|effect| format!("{effect:?}"));
    effects.dedup();
    effects
}

pub fn analyze_pipeline(pipeline: &ParsedPipeline) -> Vec<Effect> {
    let mut effects = Vec::new();

    if pipeline.commands.len() > 1 {
        effects.push(Effect::PipelineDataFlow);

        let has_network_source = pipeline.commands[..pipeline.commands.len() - 1]
            .iter()
            .any(|cmd| {
                matches!(
                    cmd.program.as_str(),
                    "curl" | "wget" | "nc" | "netcat" | "fetch" | "http"
                )
            });
        if has_network_source {
            for cmd in &pipeline.commands[1..] {
                if matches!(
                    cmd.program.as_str(),
                    "bash" | "sh" | "zsh" | "dash" | "python" | "python3" | "perl" | "ruby" | "node"
                ) {
                    effects.push(Effect::UntrustedPipelineExecution(cmd.program.clone()));
                }
            }
        }
    }

    for command in &pipeline.commands {
        effects.push(Effect::ProcessCreation);

        if matches!(
            command.program.as_str(),
            "curl" | "wget" | "ssh" | "scp" | "nc" | "netcat"
        ) {
            effects.push(Effect::NetworkAccess);
        }
        if matches!(command.program.as_str(), "sudo" | "su" | "doas" | "pkexec") {
            effects.push(Effect::PrivilegeChange);
        }
        if matches!(command.program.as_str(), "kill" | "killall" | "pkill" | "xkill") {
            let target = command
                .args
                .iter()
                .find(|arg| !arg.starts_with('-'))
                .cloned()
                .unwrap_or_else(|| "process".into());
            effects.push(Effect::ProcessKill(target));
        }
        if matches!(
            command.program.as_str(),
            "apt" | "apt-get" | "pacman" | "dnf" | "yum"
        ) {
            let is_install = command
                .args
                .iter()
                .any(|arg| arg == "install" || arg == "-S");
            if is_install {
                effects.push(Effect::PackageInstallation(command.program.clone()));
            }
        } else if matches!(
            command.program.as_str(),
            "cargo" | "npm" | "pip" | "pip3" | "gem"
        ) {
            let is_install = command
                .args
                .iter()
                .any(|arg| arg == "install" || arg == "add");
            if is_install {
                effects.push(Effect::PackageInstallation(command.program.clone()));
            }
        }
        if matches!(command.program.as_str(), "rm" | "shred" | "unlink") {
            for path in deletion_targets(&command.args) {
                if is_sensitive_path(&path) {
                    effects.push(Effect::SensitivePathWrite(path));
                } else {
                    effects.push(Effect::FilesystemDelete(path));
                }
            }
        }
        if command.program == "dd" {
            for arg in &command.args {
                if let Some(target) = arg.strip_prefix("of=") {
                    if is_sensitive_path(target) {
                        effects.push(Effect::SensitivePathWrite(target.to_string()));
                    } else {
                        effects.push(Effect::FilesystemWrite(target.to_string()));
                    }
                }
            }
        }
        if matches!(command.program.as_str(), "chmod" | "chown") {
            let is_recursive = command.args.iter().any(|arg| {
                arg == "-R"
                    || arg == "--recursive"
                    || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('R'))
            });
            if is_recursive {
                let positional: Vec<&str> = command
                    .args
                    .iter()
                    .filter(|arg| !arg.starts_with('-'))
                    .map(|s| s.as_str())
                    .collect();
                if positional.len() > 1 {
                    for &target in &positional[1..] {
                        if is_sensitive_path(target) {
                            effects.push(Effect::SensitivePathWrite(target.to_string()));
                        } else {
                            effects.push(Effect::FilesystemWrite(target.to_string()));
                        }
                    }
                }
            }
        }

        if matches!(command.program.as_str(), "cat" | "head" | "tail" | "wc" | "less" | "more") {
            for arg in &command.args {
                if !arg.starts_with('-') {
                    effects.push(classify_read(arg));
                }
            }
        }

        for redirect in &command.redirects {
            match redirect {
                Redirection::Stdin(path) => effects.push(classify_read(path)),
                Redirection::Stdout(path)
                | Redirection::StdoutAppend(path)
                | Redirection::Stderr(path)
                | Redirection::StderrAppend(path)
                | Redirection::StdoutAndStderr(path)
                | Redirection::StdoutAndStderrAppend(path) => effects.push(classify_write(path)),
                Redirection::HereString(_) => {}
                Redirection::DupRead(_, _) | Redirection::DupWrite(_, _) => {}
            }
        }

        fn classify_read(path: &str) -> Effect {
            if is_sensitive_path(path) {
                Effect::SensitivePathRead(path.to_owned())
            } else {
                Effect::FilesystemRead(path.to_owned())
            }
        }

        fn classify_write(path: &str) -> Effect {
            if is_sensitive_path(path) {
                Effect::SensitivePathWrite(path.to_owned())
            } else {
                Effect::FilesystemWrite(path.to_owned())
            }
        }

        fn deletion_targets(args: &[String]) -> impl Iterator<Item = String> + '_ {
            let mut options_ended = false;
            args.iter().filter_map(move |arg| {
                if !options_ended && arg == "--" {
                    options_ended = true;
                    return None;
                }
                if !options_ended && arg.starts_with('-') {
                    return None;
                }
                Some(arg.clone())
            })
        }
    }

    effects.sort_by_key(|effect| format!("{effect:?}"));
    effects.dedup();
    effects
}

pub fn is_sensitive_path(path: &str) -> bool {
    let path_obj = std::path::Path::new(path);
    if path_obj == std::path::Path::new("/") {
        return true;
    }
    if path_obj.is_absolute() {
        path_obj.starts_with("/etc")
            || path_obj.starts_with("/boot")
            || path_obj.starts_with("/sys")
            || path_obj.starts_with("/proc")
            || path_obj.starts_with("/dev")
            || path_obj.starts_with("/usr")
            || path_obj.starts_with("/bin")
            || path_obj.starts_with("/sbin")
            || path_obj.starts_with("/lib")
            || path_obj.starts_with("/lib64")
            || path_obj.starts_with("/root")
            || path_obj.starts_with("/var/lib")
            || path_obj
                .components()
                .any(|component| component.as_os_str() == ".ssh" || component.as_os_str() == ".gnupg")
    } else {
        path_obj
            .components()
            .any(|component| component.as_os_str() == ".ssh" || component.as_os_str() == ".gnupg")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_pipeline;

    #[test]
    fn identifies_network_delete_and_pipeline_effects() {
        let pipeline = parse_pipeline("curl example.test | rm -rf build")
            .unwrap()
            .unwrap();

        let effects = analyze_pipeline(&pipeline);

        assert!(effects.contains(&Effect::NetworkAccess));
        assert!(effects.contains(&Effect::FilesystemDelete("build".into())));
        assert!(effects.contains(&Effect::PipelineDataFlow));
    }

    #[test]
    fn identifies_redirection_writes() {
        let pipeline = parse_pipeline("echo data > output.txt").unwrap().unwrap();

        assert!(
            analyze_pipeline(&pipeline).contains(&Effect::FilesystemWrite("output.txt".into()))
        );
    }

    #[test]
    fn identifies_sensitive_paths_and_skips_rm_options() {
        let pipeline = parse_pipeline("rm -rf -- /etc/melchior.conf")
            .unwrap()
            .unwrap();
        let effects = analyze_pipeline(&pipeline);

        assert!(effects.contains(&Effect::SensitivePathWrite("/etc/melchior.conf".into())));
    }

    #[test]
    fn identifies_dd_sensitive_output() {
        let pipeline = parse_pipeline("dd if=/dev/zero of=/dev/sda bs=1M count=1")
            .unwrap()
            .unwrap();
        let effects = analyze_pipeline(&pipeline);

        assert!(effects.contains(&Effect::SensitivePathWrite("/dev/sda".into())));
    }

    #[test]
    fn identifies_chmod_recursive_sensitive_output() {
        let pipeline = parse_pipeline("chmod -R 777 /var/lib/data")
            .unwrap()
            .unwrap();
        let effects = analyze_pipeline(&pipeline);

        assert!(effects.contains(&Effect::SensitivePathWrite("/var/lib/data".into())));
    }

    #[test]
    fn identifies_root_deletion_as_sensitive() {
        let pipeline = parse_pipeline("rm -rf /")
            .unwrap()
            .unwrap();
        let effects = analyze_pipeline(&pipeline);

        assert!(effects.contains(&Effect::SensitivePathWrite("/".into())));
    }

    #[test]
    fn calculates_blast_radius_for_files_and_directories() {
        let temp_dir = std::env::temp_dir().join(format!("melchior-test-blast-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();
        let file1 = temp_dir.join("file1.txt");
        let file2 = temp_dir.join("file2.txt");
        std::fs::write(&file1, "hello world").unwrap();
        std::fs::write(&file2, "1234567890").unwrap();

        let radius = calculate_blast_radius_for_path(temp_dir.to_str().unwrap()).unwrap();
        assert_eq!(radius.file_count, 2);
        assert_eq!(radius.total_bytes, 21);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn identifies_untrusted_pipeline_execution() {
        let pipeline = parse_pipeline("curl -sSL https://example.com/install.sh | bash")
            .unwrap()
            .unwrap();
        let effects = analyze_pipeline(&pipeline);
        assert!(effects.iter().any(|e| matches!(e, Effect::UntrustedPipelineExecution(cmd) if cmd == "bash")));
    }

    #[test]
    fn identifies_package_installation_and_process_kill() {
        let p1 = parse_pipeline("cargo install ripgrep").unwrap().unwrap();
        let e1 = analyze_pipeline(&p1);
        assert!(e1.contains(&Effect::PackageInstallation("cargo".into())));

        let p2 = parse_pipeline("kill -9 1234").unwrap().unwrap();
        let e2 = analyze_pipeline(&p2);
        assert!(e2.contains(&Effect::ProcessKill("1234".into())));
    }
}
