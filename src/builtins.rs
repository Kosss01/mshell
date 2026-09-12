use std::env;
use std::path::PathBuf;

pub(crate) fn builtin_print(s: &str) {
    let bytes = s.as_bytes();
    unsafe {
        libc::write(libc::STDOUT_FILENO, bytes.as_ptr() as *const libc::c_void, bytes.len());
    }
}

pub(crate) fn builtin_println(s: &str) {
    let bytes = s.as_bytes();
    unsafe {
        libc::write(libc::STDOUT_FILENO, bytes.as_ptr() as *const libc::c_void, bytes.len());
        libc::write(libc::STDOUT_FILENO, b"\n".as_ptr() as *const libc::c_void, 1);
    }
}

pub enum BuiltinResult {
    Handled,
    Status(i32),
    Exit(i32),
}

pub const BUILTIN_NAMES: &[&str] = &[
    ":",
    "cd",
    "pwd",
    "echo",
    "printf",
    "true",
    "false",
    "export",
    "unset",
    "env",
    "exit",
    "type",
    "command",
    "jobs",
    "wait",
    "kill",
    "fg",
    "bg",
    "history",
    "timeline",
    "explain",
    "policy",
    "processes",
    "network",
    "ports",
    "connections",
    "children",
    "json",
    "yaml",
    "toml",
    "help",
    "abbr",
    "alias",
    "unalias",
    "tutor",
    "whatif",
    "undo",
    "snapshot",
    "tree",
    "cheat",
    "doctor",
    "service",
    "curl",
    "cadet",
    "profile",
    "drill",
    "db",
    "ping",
    "netstat",
    "tour",
    "explore",
];

pub fn is_builtin(command: &str) -> bool {
    BUILTIN_NAMES.contains(&command)
}

pub fn execute(
    command: &str,
    args: &[String],
    executable_lookup: impl Fn(&str) -> Option<PathBuf>,
) -> Result<BuiltinResult, String> {
    match command {
        "pwd" => pwd(args),
        ":" => noop(args),
        "cd" => cd(args),
        "echo" => echo(args),
        "printf" => printf(args),
        "true" => status_builtin("true", args, true),
        "false" => status_builtin("false", args, false),
        "export" => export(args),
        "unset" => unset(args),
        "env" => env_builtin(args),
        "exit" => exit(args),
        "type" => type_command(args, executable_lookup),
        "command" => command_lookup(args, executable_lookup),

        _ => Err(format!("unknown builtin: {command}")),
    }
}

fn exit(args: &[String]) -> Result<BuiltinResult, String> {
    let status = match args {
        [] => 0,
        [status] => status
            .parse::<i32>()
            .map_err(|_| format!("exit: numeric argument required: {status}"))?,
        _ => return Err("exit: too many arguments".into()),
    };

    if !(0..=255).contains(&status) {
        return Err(format!("exit: status out of range: {status}"));
    }

    Ok(BuiltinResult::Exit(status))
}

fn noop(_args: &[String]) -> Result<BuiltinResult, String> {
    Ok(BuiltinResult::Handled)
}

fn pwd(args: &[String]) -> Result<BuiltinResult, String> {
    if !args.is_empty() {
        return Err("pwd: too many arguments".into());
    }
    match env::current_dir() {
        Ok(path) => {
            builtin_println(&path.display().to_string());
            Ok(BuiltinResult::Handled)
        }

        Err(error) => Err(format!("pwd: error getting current directory: {error}")),
    }
}

fn cd(args: &[String]) -> Result<BuiltinResult, String> {
    let previous = env::current_dir().map_err(|error| format!("cd: {error}"))?;
    let target = match args {
        [] => env::var("HOME").map_err(|_| "cd: HOME is not set".to_string())?,

        [value] if value == "-" => {
            env::var("OLDPWD").map_err(|_| "cd: OLDPWD is not set".to_string())?
        }

        [target] => expand_home(target)?,

        _ => {
            return Err("cd: too many arguments".into());
        }
    };

    if let Err(error) = env::set_current_dir(&target) {
        return Err(format!("cd: {target}: {error}"));
    }

    let current = env::current_dir().map_err(|error| format!("cd: {error}"))?;
    unsafe {
        env::set_var("OLDPWD", previous);
        env::set_var("PWD", &current);
    }
    if matches!(args, [value] if value == "-") {
        builtin_println(&current.display().to_string());
    }

    Ok(BuiltinResult::Handled)
}

fn expand_home(path: &str) -> Result<String, String> {
    let expanded = expand_environment_variables(path);

    if expanded == "~" {
        return env::var("HOME").map_err(|_| "cd: HOME is not set".to_string());
    }

    if let Some(rest) = expanded.strip_prefix("~/") {
        let home = env::var("HOME").map_err(|_| "cd: HOME is not set".to_string())?;

        return Ok(format!("{home}/{rest}"));
    }

    Ok(expanded)
}

fn expand_environment_variables(input: &str) -> String {
    let mut result = String::new();
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '$' {
            result.push(ch);
            continue;
        }

        let mut name = String::new();
        match chars.peek() {
            Some('{') => {
                chars.next();
                for next in chars.by_ref() {
                    if next == '}' {
                        break;
                    }
                    name.push(next);
                }
            }
            Some(next) if next.is_ascii_alphabetic() || *next == '_' => {
                while let Some(next) = chars.peek().copied() {
                    if next.is_ascii_alphanumeric() || next == '_' {
                        name.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }
            }
            _ => {
                result.push('$');
                continue;
            }
        }

        if name.is_empty() {
            result.push('$');
            continue;
        }

        result.push_str(&env::var(&name).unwrap_or_default());
    }

    result
}

fn echo(args: &[String]) -> Result<BuiltinResult, String> {
    let (output, newline) = echo_output(args);
    if newline {
        builtin_println(&output);
    } else {
        builtin_print(&output);
    }
    Ok(BuiltinResult::Handled)
}

fn echo_output(args: &[String]) -> (String, bool) {
    let mut arguments = args;
    let mut newline = true;
    while let Some(first) = arguments.first() {
        if first != "-n" {
            break;
        }
        newline = false;
        arguments = &arguments[1..];
    }
    (arguments.join(" "), newline)
}

fn printf(args: &[String]) -> Result<BuiltinResult, String> {
    let Some(format) = args.first() else {
        return Err("printf: format required".into());
    };
    let mut output = String::new();
    let mut arguments = args[1..].iter();
    let mut chars = format.chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            match chars.next() {
                Some('n') => output.push('\n'),
                Some('t') => output.push('\t'),
                Some('\\') => output.push('\\'),
                Some(other) => {
                    output.push('\\');
                    output.push(other);
                }
                None => output.push('\\'),
            }
        } else if character != '%' {
            output.push(character);
        } else {
            match chars.next() {
                Some('%') => output.push('%'),
                Some('s') => output.push_str(arguments.next().map(String::as_str).unwrap_or("")),
                Some('d') => {
                    let value = arguments.next().map(String::as_str).unwrap_or("0");
                    if value.parse::<i64>().is_err() {
                        return Err(format!("printf: invalid integer: {value}"));
                    }
                    output.push_str(value);
                }
                Some(specifier) => {
                    return Err(format!("printf: unsupported format %{specifier}"));
                }
                None => return Err("printf: incomplete format".into()),
            }
        }
    }
    builtin_print(&output);
    Ok(BuiltinResult::Handled)
}

fn status_builtin(command: &str, args: &[String], success: bool) -> Result<BuiltinResult, String> {
    if !args.is_empty() {
        return Err(format!("{command}: too many arguments"));
    }
    Ok(BuiltinResult::Status(if success { 0 } else { 1 }))
}

fn export(args: &[String]) -> Result<BuiltinResult, String> {
    if args.is_empty() {
        for (name, value) in sorted_environment() {
            builtin_println(&format!("{name}={value}"));
        }
        return Ok(BuiltinResult::Handled);
    }

    let assignments = args
        .iter()
        .map(|assignment| {
            let Some((name, value)) = assignment.split_once('=') else {
                return Err(format!("export: expected NAME=VALUE: {assignment}"));
            };
            if !is_valid_variable_name(name) {
                return Err(format!("export: invalid variable name: {name}"));
            }
            Ok((name, value))
        })
        .collect::<Result<Vec<_>, _>>()?;

    for (name, value) in assignments {
        // The shell mutates its process environment from its single REPL thread.
        unsafe { env::set_var(name, value) };
    }
    Ok(BuiltinResult::Handled)
}

fn unset(args: &[String]) -> Result<BuiltinResult, String> {
    if args.is_empty() {
        return Err("unset: variable name required".into());
    }

    let names: Vec<&String> = args.iter().filter(|a| *a != "-v" && *a != "-f").collect();
    if names.is_empty() {
        return Err("unset: variable name required".into());
    }

    for name in &names {
        if !is_valid_variable_name(name) {
            return Err(format!("unset: invalid variable name: {name}"));
        }
    }

    for name in names {
        unsafe { env::remove_var(name) };
    }
    Ok(BuiltinResult::Handled)
}

fn env_builtin(args: &[String]) -> Result<BuiltinResult, String> {
    if !args.is_empty() {
        return Err("env: arguments are not supported".into());
    }
    for (name, value) in sorted_environment() {
        builtin_println(&format!("{name}={value}"));
    }
    Ok(BuiltinResult::Handled)
}

fn sorted_environment() -> Vec<(String, String)> {
    let mut variables = env::vars().collect::<Vec<_>>();
    variables.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    variables
}

fn is_valid_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
        && chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn type_command(
    args: &[String],
    executable_lookup: impl Fn(&str) -> Option<PathBuf>,
) -> Result<BuiltinResult, String> {
    if args.is_empty() {
        return Err("type: missing argument".into());
    }

    let mut all_found = true;
    for command in args {
        if is_builtin(command) {
            builtin_println(&format!("{command} is a shell builtin"));
        } else if let Some(path) = executable_lookup(command) {
            builtin_println(&format!("{command} is {}", path.display()));
        } else {
            builtin_println(&format!("{command}: not found"));
            all_found = false;
        }

    }

    Ok(if all_found {
        BuiltinResult::Handled
    } else {
        BuiltinResult::Status(1)
    })
}

fn command_lookup(
    args: &[String],
    executable_lookup: impl Fn(&str) -> Option<PathBuf>,
) -> Result<BuiltinResult, String> {
    let (verbose, names) = match args {
        [flag, names @ ..] if flag == "-v" || flag == "-V" => (true, names),
        names => (false, names),
    };
    if names.is_empty() {
        return Err("command: name required".into());
    }

    let mut all_found = true;
    for name in names {
        if is_builtin(name) {
            if verbose {
                builtin_println(&format!("{name} is a shell builtin"));
            } else {
                builtin_println(name);
            }
        } else if let Some(path) = executable_lookup(name) {
            if verbose {
                builtin_println(&format!("{name} is {}", path.display()));
            } else {
                builtin_println(&path.display().to_string());
            }
        } else {
            all_found = false;
        }
    }
    Ok(BuiltinResult::Status(if all_found { 0 } else { 1 }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exit_status() {
        let result = execute("exit", &["42".into()], |_| None).unwrap();
        assert!(matches!(result, BuiltinResult::Exit(42)));
    }

    #[test]
    fn provides_noop_builtin() {
        assert!(matches!(
            execute(":", &["ignored".into()], |_| None).unwrap(),
            BuiltinResult::Handled
        ));
        assert!(is_builtin(":"));
    }

    #[test]
    fn command_reports_builtin_and_missing_status() {
        let result = execute(
            "command",
            &["-v".into(), "echo".into(), "definitely-missing".into()],
            |_| None,
        )
        .unwrap();

        assert!(matches!(result, BuiltinResult::Status(1)));
    }

    #[test]
    fn rejects_invalid_exit_status() {
        assert!(execute("exit", &["nope".into()], |_| None).is_err());
        assert!(execute("exit", &["256".into()], |_| None).is_err());
    }

    #[test]
    fn exports_environment_assignments() {
        let name = "MELCHIOR_TEST_EXPORT";
        unsafe { env::remove_var(name) };
        let result = execute("export", &[format!("{name}=enabled")], |_| None).unwrap();
        assert!(matches!(result, BuiltinResult::Handled));
        assert_eq!(env::var(name).unwrap(), "enabled");
        unsafe { env::remove_var(name) };
    }

    #[test]
    fn rejects_invalid_export_names() {
        assert!(execute("export", &["not-valid=value".into()], |_| None).is_err());
        assert!(execute("export", &["missing-equals".into()], |_| None).is_err());
    }

    #[test]
    fn export_does_not_partially_apply_invalid_assignments() {
        let name = "MELCHIOR_TEST_EXPORT_ATOMIC";
        unsafe { env::remove_var(name) };
        assert!(
            execute(
                "export",
                &[format!("{name}=enabled"), "not-valid=value".into()],
                |_| None
            )
            .is_err()
        );
        assert!(env::var(name).is_err());
    }

    #[test]
    fn unsets_environment_variables() {
        let name = "MELCHIOR_TEST_UNSET";
        unsafe { env::set_var(name, "value") };
        let result = execute("unset", &[name.into()], |_| None).unwrap();
        assert!(matches!(result, BuiltinResult::Handled));
        assert!(env::var(name).is_err());
    }

    #[test]
    fn rejects_invalid_unset_names() {
        assert!(execute("unset", &["not-valid".into()], |_| None).is_err());
        assert!(execute("unset", &[], |_| None).is_err());
    }

    #[test]
    fn echo_supports_n_without_consuming_regular_arguments() {
        assert_eq!(
            echo_output(&["-n".into(), "hello".into(), "world".into()]),
            ("hello world".into(), false)
        );
        assert_eq!(
            echo_output(&["hello".into(), "-n".into()]),
            ("hello -n".into(), true)
        );
    }

    #[test]
    fn cd_dash_switches_to_oldpwd_and_updates_directory_variables() {
        let original_directory = env::current_dir().unwrap();
        let original_pwd = env::var_os("PWD");
        let original_oldpwd = env::var_os("OLDPWD");
        let target = env::temp_dir().join(format!("melchior-cd-{}", std::process::id()));
        std::fs::create_dir_all(&target).unwrap();

        unsafe {
            env::set_var("PWD", &original_directory);
            env::set_var("OLDPWD", &original_directory);
        }
        execute("cd", &[target.to_string_lossy().into_owned()], |_| None).unwrap();
        assert_eq!(env::current_dir().unwrap(), target);
        assert_eq!(
            env::var_os("OLDPWD").as_deref(),
            Some(original_directory.as_os_str())
        );

        execute("cd", &["-".into()], |_| None).unwrap();
        assert_eq!(env::current_dir().unwrap(), original_directory);
        assert_eq!(
            env::var_os("PWD").as_deref(),
            Some(original_directory.as_os_str())
        );

        env::set_current_dir(&original_directory).unwrap();
        unsafe {
            match original_pwd {
                Some(value) => env::set_var("PWD", value),
                None => env::remove_var("PWD"),
            }
            match original_oldpwd {
                Some(value) => env::set_var("OLDPWD", value),
                None => env::remove_var("OLDPWD"),
            }
        }
        std::fs::remove_dir(&target).unwrap();
    }

    #[test]
    fn type_accepts_multiple_commands() {
        let result = execute(
            "type",
            &["echo".into(), "definitely-missing".into()],
            |_| None,
        )
        .unwrap();

        assert!(matches!(result, BuiltinResult::Status(1)));
    }

    #[test]
    fn type_returns_success_when_all_commands_are_found() {
        let result = execute("type", &["echo".into(), "pwd".into()], |_| None).unwrap();

        assert!(matches!(result, BuiltinResult::Handled));
    }

    #[test]
    fn rejects_pwd_arguments() {
        assert!(execute("pwd", &["unexpected".into()], |_| None).is_err());
    }

    #[test]
    fn unset_does_not_partially_apply_invalid_names() {
        let name = "MELCHIOR_TEST_UNSET_ATOMIC";
        unsafe { env::set_var(name, "value") };
        assert!(execute("unset", &[name.into(), "not-valid".into()], |_| None).is_err());
        assert_eq!(env::var(name).unwrap(), "value");
        unsafe { env::remove_var(name) };
    }

    #[test]
    fn lists_environment_with_env_builtin() {
        unsafe { env::set_var("MELCHIOR_TEST_ENV", "visible") };
        assert!(matches!(
            execute("env", &[], |_| None).unwrap(),
            BuiltinResult::Handled
        ));
        unsafe { env::remove_var("MELCHIOR_TEST_ENV") };
    }

    #[test]
    fn sorts_environment_names() {
        let name_a = "MELCHIOR_TEST_SORT_A";
        let name_z = "MELCHIOR_TEST_SORT_Z";
        unsafe {
            env::set_var(name_z, "z");
            env::set_var(name_a, "a");
        }

        let variables = sorted_environment();
        let position_a = variables.iter().position(|(name, _)| name == name_a);
        let position_z = variables.iter().position(|(name, _)| name == name_z);
        assert!(position_a < position_z);

        unsafe {
            env::remove_var(name_a);
            env::remove_var(name_z);
        }
    }

    #[test]
    fn rejects_env_arguments() {
        assert!(execute("env", &["unexpected".into()], |_| None).is_err());
    }

    #[test]
    fn provides_standard_status_builtins() {
        assert!(matches!(
            execute("true", &[], |_| None).unwrap(),
            BuiltinResult::Status(0)
        ));
        assert!(matches!(
            execute("false", &[], |_| None).unwrap(),
            BuiltinResult::Status(1)
        ));
    }

    #[test]
    fn validates_printf_formats() {
        assert!(matches!(
            execute(
                "printf",
                &["%s=%d\\n".into(), "value".into(), "7".into()],
                |_| None
            )
            .unwrap(),
            BuiltinResult::Handled
        ));
        assert!(execute("printf", &["%q".into()], |_| None).is_err());
        assert!(execute("printf", &["%d".into(), "nope".into()], |_| None).is_err());
    }
}

