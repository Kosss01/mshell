use std::cell::RefCell;
use std::collections::BTreeSet;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Instant;

use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::builtins::{self, BuiltinResult};
use crate::effects::{Effect, analyze_pipeline};
use crate::parser::{Ast, AstSequence, LogicalOp, ParsedCommand, ParsedIf, ParsedFor, ParsedWhile, ParsedFunction, ParsedPipeline, Redirection};
use crate::policy::{self, PolicyDecision, RiskLevel};

pub enum ProcessHandle {
    External(Child),
    Forked(u32),
}

impl ProcessHandle {
    pub fn id(&self) -> u32 {
        match self {
            Self::External(child) => child.id(),
            Self::Forked(pid) => *pid,
        }
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        match self {
            Self::External(child) => child.try_wait(),
            Self::Forked(pid) => {
                let mut status = 0;
                let res = unsafe {
                    libc::waitpid(
                        *pid as libc::pid_t,
                        &mut status,
                        libc::WNOHANG | libc::WUNTRACED,
                    )
                };
                if res == -1 {
                    Err(std::io::Error::last_os_error())
                } else if res == 0 {
                    Ok(None)
                } else {
                    Ok(Some(ExitStatus::from_raw(status)))
                }
            }
        }
    }

    pub fn wait(&mut self) -> std::io::Result<ExitStatus> {
        match self {
            Self::External(child) => child.wait(),
            Self::Forked(pid) => {
                let mut status = 0;
                let res = unsafe { libc::waitpid(*pid as libc::pid_t, &mut status, libc::WUNTRACED) };
                if res == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(ExitStatus::from_raw(status))
                }
            }
        }
    }

    pub fn kill(&mut self) -> std::io::Result<()> {
        match self {
            Self::External(child) => child.kill(),
            Self::Forked(pid) => {
                let res = unsafe { libc::kill(*pid as libc::pid_t, libc::SIGKILL) };
                if res == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            }
        }
    }
}

struct Job {
    id: usize,
    command: String,
    children: Vec<ProcessHandle>,
    process_ids: Vec<u32>,
    state: JobState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobState {
    Running,
    Stopped,
}

pub struct Executor {
    jobs: RefCell<Vec<Job>>,
    next_job_id: RefCell<usize>,
    history: RefCell<Vec<String>>,
    timeline: RefCell<Vec<ExecutionRecord>>,
    database: RefCell<Option<Connection>>,
    abbreviations: RefCell<std::collections::BTreeMap<String, String>>,
    aliases: RefCell<std::collections::BTreeMap<String, String>>,
    functions: RefCell<std::collections::BTreeMap<String, ParsedFunction>>,
    completion_extractor: crate::completions::HelpCompletionExtractor,
    interactive: std::cell::Cell<bool>,
    pub tutor: RefCell<Option<crate::tutor::TutorEngine>>,
    pub sandbox: RefCell<Option<crate::sandbox::SandboxManager>>,
    pub server_sim: RefCell<crate::server_sim::ServerSimulator>,
    pub cadet: RefCell<crate::cadet::CadetProfile>,
    pub drills: RefCell<crate::drills::DrillEngine>,
    pub last_failed_cmd: RefCell<Option<(String, i32)>>,
}

fn default_abbreviations() -> std::collections::BTreeMap<String, String> {
    let mut map = std::collections::BTreeMap::new();
    map.insert("gco".into(), "git checkout".into());
    map.insert("gst".into(), "git status".into());
    map.insert("gp".into(), "git push".into());
    map.insert("gl".into(), "git pull".into());
    map.insert("gd".into(), "git diff".into());
    map.insert("gc".into(), "git commit".into());
    map
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionRecord {
    pub command: String,
    pub working_directory: PathBuf,
    pub duration_ms: u128,
    pub status: i32,
    pub environment_keys: Vec<String>,
    pub redirections: Vec<Redirection>,
    pub stages: Vec<ProcessRecord>,
    pub effects: Vec<Effect>,
    pub policy: PolicyDecision,
    pub risk: RiskLevel,
    pub audit: AuditEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOutcome {
    Executed,
    Blocked,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditEvent {
    pub decision: PolicyDecision,
    pub outcome: AuditOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRecord {
    pub program: String,
    pub process_id: Option<u32>,
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

impl Executor {
    pub fn new() -> Self {
        crate::parser::register_subshell_executor(execute_capture);
        Self {
            jobs: RefCell::new(Vec::new()),
            next_job_id: RefCell::new(1),
            history: RefCell::new(Vec::new()),
            timeline: RefCell::new(Vec::new()),
            database: RefCell::new(None),
            abbreviations: RefCell::new(default_abbreviations()),
            aliases: RefCell::new(std::collections::BTreeMap::new()),
            functions: RefCell::new(std::collections::BTreeMap::new()),
            completion_extractor: crate::completions::HelpCompletionExtractor::new(),
            interactive: std::cell::Cell::new(false),
            tutor: RefCell::new(None),
            sandbox: RefCell::new(None),
            server_sim: RefCell::new(crate::server_sim::ServerSimulator::new()),
            cadet: RefCell::new(crate::cadet::CadetProfile::new(None)),
            drills: RefCell::new(crate::drills::DrillEngine::new()),
            last_failed_cmd: RefCell::new(None),
        }
    }

    pub fn set_interactive(&self, interactive: bool) {
        self.interactive.set(interactive);
    }

    #[allow(dead_code)]
    pub fn is_interactive(&self) -> bool {
        self.interactive.get()
    }

    pub fn persistent() -> Self {
        let executor = Self::new();
        match open_history_database() {
            Ok(database) => {
                executor.load_history(&database);
                executor.load_timeline(&database);
                *executor.cadet.borrow_mut() = crate::cadet::CadetProfile::new(Some(&database));
                *executor.database.borrow_mut() = Some(database);
            }
            Err(error) => eprintln!("shellpilot: persistent history unavailable: {error}"),
        }
        executor
    }

    pub fn history(&self) -> Vec<String> {
        self.history.borrow().clone()
    }

    pub fn abbreviations(&self) -> std::collections::BTreeMap<String, String> {
        self.abbreviations.borrow().clone()
    }

    pub fn set_abbreviation(&self, name: String, expansion: String) {
        self.abbreviations.borrow_mut().insert(name, expansion);
    }

    pub fn remove_abbreviation(&self, name: &str) -> bool {
        self.abbreviations.borrow_mut().remove(name).is_some()
    }

    fn abbr(&self, args: &[String]) -> ExecutionResult {
        if args.is_empty() || args[0] == "list" {
            for (name, expansion) in self.abbreviations.borrow().iter() {
                println!("{name} -> {expansion}");
            }
            return ExecutionResult::Builtin;
        }

        match args[0].as_str() {
            "add" => {
                if args.len() < 3 {
                    eprintln!("abbr: add requires <name> <expansion>");
                    return ExecutionResult::BuiltinStatus(1);
                }
                let name = args[1].clone();
                let expansion = args[2..].join(" ");
                self.set_abbreviation(name, expansion);
                ExecutionResult::Builtin
            }
            "remove" | "rm" => {
                if args.len() < 2 {
                    eprintln!("abbr: remove requires <name>");
                    return ExecutionResult::BuiltinStatus(1);
                }
                if !self.remove_abbreviation(&args[1]) {
                    eprintln!("abbr: no abbreviation '{}'", args[1]);
                    return ExecutionResult::BuiltinStatus(1);
                }
                ExecutionResult::Builtin
            }
            "clear" => {
                self.abbreviations.borrow_mut().clear();
                ExecutionResult::Builtin
            }
            _ => {
                eprintln!("usage: abbr [add <name> <expansion> | remove <name> | list | clear]");
                ExecutionResult::BuiltinStatus(1)
            }
        }
    }

    pub fn aliases(&self) -> std::collections::BTreeMap<String, String> {
        self.aliases.borrow().clone()
    }

    #[allow(dead_code)]
    pub fn set_alias(&self, name: String, value: String) {
        self.aliases.borrow_mut().insert(name, value);
    }

    #[allow(dead_code)]
    pub fn remove_alias(&self, name: &str) -> bool {
        self.aliases.borrow_mut().remove(name).is_some()
    }

    pub fn functions(&self) -> std::collections::BTreeMap<String, ParsedFunction> {
        self.functions.borrow().clone()
    }

    #[allow(dead_code)]
    pub fn remove_function(&self, name: &str) -> bool {
        self.functions.borrow_mut().remove(name).is_some()
    }

    pub fn type_command(&self, args: &[String]) -> ExecutionResult {
        if args.is_empty() {
            eprintln!("type: missing argument");
            return ExecutionResult::BuiltinStatus(1);
        }

        let mut all_found = true;
        for command in args {
            if let Some(val) = self.aliases.borrow().get(command) {
                println!("{command} is an alias for '{val}'");
            } else if self.functions.borrow().contains_key(command) {
                println!("{command} is a shell function");
            } else if builtins::is_builtin(command) {
                println!("{command} is a shell builtin");
            } else if let Some(path) = find_executable_in_path(command) {
                println!("{command} is {}", path.display());
            } else {
                println!("{command}: not found");
                all_found = false;
            }
        }

        if all_found {
            ExecutionResult::Builtin
        } else {
            ExecutionResult::BuiltinStatus(1)
        }
    }

    pub fn alias(&self, args: &[String]) -> ExecutionResult {
        if args.is_empty() {
            for (name, val) in self.aliases.borrow().iter() {
                println!("alias {name}='{val}'");
            }
            return ExecutionResult::Builtin;
        }

        let mut all_ok = true;
        for arg in args {
            if let Some((name, val)) = arg.split_once('=') {
                if !crate::parser::is_valid_variable_name(name) {
                    eprintln!("alias: `{name}': invalid alias name");
                    all_ok = false;
                    continue;
                }
                let clean_val = val.trim_matches('\'').trim_matches('"').to_string();
                self.aliases.borrow_mut().insert(name.to_string(), clean_val);
            } else {
                if let Some(val) = self.aliases.borrow().get(arg) {
                    println!("alias {arg}='{val}'");
                } else {
                    eprintln!("alias: {arg}: not found");
                    all_ok = false;
                }
            }
        }
        if all_ok {
            ExecutionResult::Builtin
        } else {
            ExecutionResult::BuiltinStatus(1)
        }
    }

    pub fn unalias(&self, args: &[String]) -> ExecutionResult {
        if args.is_empty() {
            eprintln!("unalias: usage: unalias [-a] name [name ...]");
            return ExecutionResult::BuiltinStatus(1);
        }

        if args.contains(&"-a".to_string()) {
            self.aliases.borrow_mut().clear();
            return ExecutionResult::Builtin;
        }

        let mut all_ok = true;
        for arg in args {
            if self.aliases.borrow_mut().remove(arg).is_none() {
                eprintln!("unalias: {arg}: not found");
                all_ok = false;
            }
        }
        if all_ok {
            ExecutionResult::Builtin
        } else {
            ExecutionResult::BuiltinStatus(1)
        }
    }

    pub fn unset(&self, args: &[String]) -> ExecutionResult {
        if args.is_empty() {
            eprintln!("unset: not enough arguments");
            return ExecutionResult::BuiltinStatus(1);
        }
        let mut unset_functions = false;
        let mut unset_vars = false;
        let mut names = Vec::new();

        for arg in args {
            if arg == "-f" {
                unset_functions = true;
            } else if arg == "-v" {
                unset_vars = true;
            } else if arg.starts_with('-') {
                eprintln!("unset: invalid option: {arg}");
                return ExecutionResult::BuiltinStatus(1);
            } else {
                names.push(arg.clone());
            }
        }

        if names.is_empty() {
            eprintln!("unset: variable or function name required");
            return ExecutionResult::BuiltinStatus(1);
        }

        if !unset_functions && !unset_vars {
            unset_vars = true;
            unset_functions = true;
        }

        for name in &names {
            if !crate::parser::is_valid_variable_name(name) {
                eprintln!("unset: invalid variable name: {name}");
                return ExecutionResult::BuiltinStatus(1);
            }
        }

        if unset_functions {
            let mut funcs = self.functions.borrow_mut();
            for name in &names {
                funcs.remove(name);
            }
        }

        if unset_vars {
            for name in &names {
                unsafe { std::env::remove_var(name) };
            }
        }

        ExecutionResult::Builtin
    }

    fn filter_history_by_dir(&self, target_dir: &std::path::Path) -> ExecutionResult {
        let mut found = 0;
        for (index, record) in self.timeline.borrow().iter().enumerate() {
            if record.working_directory == target_dir || record.working_directory.starts_with(target_dir) {
                println!("{:>5}  [{}] {} (cwd: {})", index + 1, record.status, record.command, record.working_directory.display());
                found += 1;
            }
        }
        if found == 0 {
            println!("(no history found for directory {})", target_dir.display());
        }
        ExecutionResult::Builtin
    }

    fn filter_history_by_status(&self, status_code: Option<i32>, filter_failed: bool) -> ExecutionResult {
        let mut found = 0;
        for (index, record) in self.timeline.borrow().iter().enumerate() {
            let matched = if filter_failed {
                record.status != 0
            } else if let Some(code) = status_code {
                record.status == code
            } else {
                false
            };
            if matched {
                println!("{:>5}  [{}] {} (cwd: {})", index + 1, record.status, record.command, record.working_directory.display());
                found += 1;
            }
        }
        if found == 0 {
            println!("(no history matching status criteria)");
        }
        ExecutionResult::Builtin
    }

    fn filter_timeline_by_dir(&self, target_dir: &std::path::Path) -> ExecutionResult {
        for (index, record) in self.timeline.borrow().iter().enumerate() {
            if record.working_directory == target_dir || record.working_directory.starts_with(target_dir) {
                let redirections = if record.redirections.is_empty() {
                    String::new()
                } else {
                    format!("  redirects={:?}", record.redirections)
                };
                let stages = record
                    .stages
                    .iter()
                    .map(|stage| format!("{}:{}", stage.program, stage.process_id.unwrap_or(0)))
                    .collect::<Vec<_>>()
                    .join(",");
                println!(
                    "{:>5}  {:>4} ms  [{}] {}  cwd={}  env={}  policy={} risk={} audit={:?} processes=[{}] effects={:?}{}",
                    index + 1,
                    record.duration_ms,
                    record.status,
                    record.command,
                    record.working_directory.display(),
                    record.environment_keys.len(),
                    record.policy,
                    record.risk,
                    record.audit.outcome,
                    stages,
                    record.effects,
                    redirections
                );
            }
        }
        ExecutionResult::Builtin
    }

    fn filter_timeline_by_status(&self, status_code: Option<i32>, filter_failed: bool) -> ExecutionResult {
        for (index, record) in self.timeline.borrow().iter().enumerate() {
            let matched = if filter_failed {
                record.status != 0
            } else if let Some(code) = status_code {
                record.status == code
            } else {
                false
            };
            if matched {
                let redirections = if record.redirections.is_empty() {
                    String::new()
                } else {
                    format!("  redirects={:?}", record.redirections)
                };
                let stages = record
                    .stages
                    .iter()
                    .map(|stage| format!("{}:{}", stage.program, stage.process_id.unwrap_or(0)))
                    .collect::<Vec<_>>()
                    .join(",");
                println!(
                    "{:>5}  {:>4} ms  [{}] {}  cwd={}  env={}  policy={} risk={} audit={:?} processes=[{}] effects={:?}{}",
                    index + 1,
                    record.duration_ms,
                    record.status,
                    record.command,
                    record.working_directory.display(),
                    record.environment_keys.len(),
                    record.policy,
                    record.risk,
                    record.audit.outcome,
                    stages,
                    record.effects,
                    redirections
                );
            }
        }
        ExecutionResult::Builtin
    }

    #[allow(dead_code)]
    pub fn timeline(&self) -> Vec<ExecutionRecord> {
        self.timeline.borrow().clone()
    }

    fn explain(&self, args: &[String]) -> ExecutionResult {
        if args.is_empty() {
            eprintln!("explain: command required");
            return ExecutionResult::Failed;
        }
        match crate::parser::parse_ast(&args.join(" "), 0) {
            Ok(Some(ast)) => {
                let explanation =
                    crate::explainer::explain_ast(&ast, Some(&self.completion_extractor));
                print!("{explanation}");
                ExecutionResult::Builtin
            }
            Ok(None) => ExecutionResult::Builtin,
            Err(error) => {
                eprintln!("explain: {error}");
                ExecutionResult::Failed
            }
        }
    }

    fn policy(&self, args: &[String]) -> ExecutionResult {
        if args.is_empty() {
            eprintln!("policy: command required");
            return ExecutionResult::Failed;
        }
        match crate::parser::parse_ast(&args.join(" "), 0) {
            Ok(Some(ast)) => {
                let effects = crate::effects::analyze_ast(&ast);
                println!(
                    "{} (risk={})",
                    policy::evaluate(&effects),
                    policy::risk_level(&effects)
                );
                for effect in effects {
                    println!("- {effect:?}");
                }
                ExecutionResult::Builtin
            }
            Ok(None) => ExecutionResult::Builtin,
            Err(error) => {
                eprintln!("policy: {error}");
                ExecutionResult::Failed
            }
        }
    }

    pub fn ensure_sandbox(&self) -> Result<PathBuf, String> {
        let needs_init = self.sandbox.borrow().is_none();
        if needs_init {
            let sb = crate::sandbox::SandboxManager::new().map_err(|e| format!("failed to initialize sandbox: {e}"))?;
            let ws = sb.workspace.clone();
            if self.interactive.get() {
                std::env::set_current_dir(&ws).map_err(|e| format!("failed to enter sandbox directory: {e}"))?;
            }
            *self.sandbox.borrow_mut() = Some(sb);
            Ok(ws)
        } else {
            Ok(self.sandbox.borrow().as_ref().unwrap().workspace.clone())
        }
    }

    pub fn current_workspace(&self) -> PathBuf {
        if let Some(ref sb) = *self.sandbox.borrow() {
            sb.workspace.clone()
        } else {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        }
    }

    pub fn ensure_tutor(&self) {
        let needs_init = self.tutor.borrow().is_none();
        if needs_init {
            let db_ref = self.database.borrow();
            let engine = crate::tutor::TutorEngine::new(db_ref.as_ref());
            *self.tutor.borrow_mut() = Some(engine);
        }
    }

    pub fn mark_tutor_lesson_completed(&self, lesson_id: &str) {
        if let Some(ref mut tutor) = *self.tutor.borrow_mut() {
            tutor.mark_completed(lesson_id, self.database.borrow().as_ref());
        }
        let mut cadet = self.cadet.borrow_mut();
        cadet.completed_lessons.insert(lesson_id.to_string());
        if let Some(new_rank) = cadet.add_xp(50, self.database.borrow().as_ref()) {
            println!("\n🎖️ \x1b[1;33m[PROMOTION!]\x1b[0m You have been promoted to \x1b[1;32m{}\x1b[0m!", new_rank.title());
        }
    }

    pub fn record_cadet_command(&self) {
        self.cadet
            .borrow_mut()
            .record_command(self.database.borrow().as_ref());
    }

    fn tutor_cmd(&self, args: &[String]) -> ExecutionResult {
        self.ensure_tutor();

        if args.is_empty() || args[0] == "list" {
            let overview = {
                let tutor = self.tutor.borrow();
                tutor.as_ref().unwrap().list_tracks_overview()
            };
            println!("{overview}");
            return ExecutionResult::Builtin;
        }

        match args[0].as_str() {
            "start" => {
                if args.len() < 2 {
                    eprintln!("tutor: start requires a lesson ID (e.g. 'tutor start nav_01')");
                    return ExecutionResult::BuiltinStatus(1);
                }
                let ws = match self.ensure_sandbox() {
                    Ok(w) => w,
                    Err(e) => {
                        eprintln!("tutor: {e}");
                        return ExecutionResult::Failed;
                    }
                };
                if self.interactive.get() {
                    let _ = std::env::set_current_dir(&ws);
                }
                let res = {
                    let mut tutor = self.tutor.borrow_mut();
                    tutor.as_mut().unwrap().start_lesson(&args[1], &ws)
                };
                match res {
                    Ok(briefing) => {
                        println!("{briefing}");
                        crate::shell::set_prompt_prefix(Some(format!("shellpilot:tutor 🎓 {}", args[1])));
                        ExecutionResult::Builtin
                    }
                    Err(err) => {
                        eprintln!("tutor: {err}");
                        ExecutionResult::Failed
                    }
                }
            }
            "check" => {
                let ws = self.current_workspace();
                let last_non_tutor = {
                    self.history
                        .borrow()
                        .iter()
                        .rev()
                        .find(|cmd| !cmd.trim().starts_with("tutor"))
                        .cloned()
                };
                let eval = {
                    let tutor = self.tutor.borrow();
                    tutor.as_ref().unwrap().evaluate_current(&ws, last_non_tutor.as_deref())
                };
                match eval {
                    Some(crate::tutor::ValidationResult::Success { feedback }) => {
                        println!("\n🎉 [OBJECTIVE COMPLETE!] {feedback}");
                        println!("   Type 'tutor next' to advance to the next challenge!\n");
                        let active_id = {
                            let tutor = self.tutor.borrow();
                            tutor.as_ref().unwrap().active_lesson_id.clone()
                        };
                        if let Some(id) = active_id {
                            self.mark_tutor_lesson_completed(&id);
                        }
                        ExecutionResult::Builtin
                    }
                    Some(crate::tutor::ValidationResult::Incomplete { reason }) => {
                        println!("⏳ [INCOMPLETE] {reason}");
                        ExecutionResult::BuiltinStatus(1)
                    }
                    Some(crate::tutor::ValidationResult::Failed { error }) => {
                        println!("❌ [FAILED] {error}");
                        ExecutionResult::BuiltinStatus(1)
                    }
                    None => {
                        println!("No active lesson. Start one with 'tutor start <id>' or type 'tutor' to see list.");
                        ExecutionResult::Builtin
                    }
                }
            }
            "hint" => {
                let hint = {
                    let mut tutor = self.tutor.borrow_mut();
                    tutor.as_mut().unwrap().next_hint()
                };
                if let Some(h) = hint {
                    println!("{h}");
                } else {
                    println!("No active lesson. Choose one with 'tutor start <id>'.");
                }
                ExecutionResult::Builtin
            }
            "solution" => {
                let sol = {
                    let tutor = self.tutor.borrow();
                    tutor.as_ref().unwrap().get_solution()
                };
                if let Some(s) = sol {
                    println!("{s}");
                } else {
                    println!("No active lesson. Choose one with 'tutor start <id>'.");
                }
                ExecutionResult::Builtin
            }
            "reset" => {
                let ws = self.current_workspace();
                if self.interactive.get() {
                    let _ = std::env::set_current_dir(&ws);
                }
                let res = {
                    let mut tutor = self.tutor.borrow_mut();
                    tutor.as_mut().unwrap().reset_lesson(&ws)
                };
                match res {
                    Ok(msg) => {
                        println!("{msg}");
                        ExecutionResult::Builtin
                    }
                    Err(err) => {
                        eprintln!("tutor: {err}");
                        ExecutionResult::Failed
                    }
                }
            }
            "next" => {
                let ws = self.current_workspace();
                if self.interactive.get() {
                    let _ = std::env::set_current_dir(&ws);
                }
                let (res, active_id) = {
                    let mut tutor = self.tutor.borrow_mut();
                    let t = tutor.as_mut().unwrap();
                    let r = t.advance_to_next(&ws);
                    let active_id = t.active_lesson_id.clone();
                    (r, active_id)
                };
                match res {
                    Ok(msg) => {
                        println!("{msg}");
                        if let Some(id) = active_id {
                            crate::shell::set_prompt_prefix(Some(format!("shellpilot:tutor 🎓 {id}")));
                        }
                        ExecutionResult::Builtin
                    }
                    Err(err) => {
                        eprintln!("tutor: {err}");
                        ExecutionResult::Failed
                    }
                }
            }
            "exit" => {
                let ws = self.current_workspace();
                if self.interactive.get() {
                    let _ = std::env::set_current_dir(&ws);
                }
                {
                    let mut tutor = self.tutor.borrow_mut();
                    tutor.as_mut().unwrap().active_lesson_id = None;
                }
                crate::shell::set_prompt_prefix(None);
                println!("Exited tutor mode.");
                ExecutionResult::Builtin
            }
            unknown => {
                eprintln!("tutor: unknown subcommand '{unknown}'. Usage: tutor [start|check|hint|solution|reset|next|list|exit]");
                ExecutionResult::BuiltinStatus(1)
            }
        }
    }

    fn whatif_cmd(&self, args: &[String]) -> ExecutionResult {
        if args.is_empty() {
            eprintln!("usage: whatif <command>");
            return ExecutionResult::BuiltinStatus(1);
        }
        let cmd = args.join(" ");
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let preview = crate::sandbox::execute_whatif(&cmd, &cwd);
        println!("{preview}");
        self.cadet.borrow_mut().record_whatif(self.database.borrow().as_ref());
        ExecutionResult::Builtin
    }

    fn undo_cmd(&self, args: &[String]) -> ExecutionResult {
        if args.first().map(|s| s.as_str()) == Some("diff") {
            let sb_borrow = self.sandbox.borrow();
            if let Some(ref sb) = *sb_borrow {
                match sb.diff_previous_snapshot() {
                    Ok(diff) => {
                        println!("{diff}");
                        return ExecutionResult::Builtin;
                    }
                    Err(err) => {
                        eprintln!("undo diff: {err}");
                        return ExecutionResult::Failed;
                    }
                }
            } else {
                eprintln!("undo diff: no active sandbox session.");
                return ExecutionResult::Failed;
            }
        }

        let mut sb_borrow = self.sandbox.borrow_mut();
        if let Some(ref mut sb) = *sb_borrow {
            match sb.undo() {
                Ok(msg) => {
                    println!("{msg}");
                    self.cadet.borrow_mut().record_undo(self.database.borrow().as_ref());
                    ExecutionResult::Builtin
                }
                Err(err) => {
                    eprintln!("undo: {err}");
                    ExecutionResult::Failed
                }
            }
        } else {
            eprintln!("undo: no active sandbox session. Start one with 'shellpilot --sandbox' or run 'snapshot'.");
            ExecutionResult::Failed
        }
    }

    fn tree_cmd(&self, args: &[String]) -> ExecutionResult {
        match crate::guidance::run_tree(args) {
            Ok(tree) => {
                println!("{tree}");
                ExecutionResult::Builtin
            }
            Err(e) => {
                eprintln!("{e}");
                ExecutionResult::Failed
            }
        }
    }

    fn cheat_cmd(&self, args: &[String]) -> ExecutionResult {
        let tool = args.first().map(|s| s.as_str());
        let text = crate::guidance::run_cheat(tool);
        println!("{text}");
        ExecutionResult::Builtin
    }

    fn doctor_cmd(&self, _args: &[String]) -> ExecutionResult {
        let last = self.last_failed_cmd.borrow();
        let (cmd, status) = match *last {
            Some((ref c, s)) => (c.clone(), s),
            None => ("".to_string(), 0),
        };
        let cwd = self.current_workspace();
        let report = crate::guidance::diagnose(&cmd, status, &cwd);
        println!("{}", report.format());
        ExecutionResult::Builtin
    }

    fn service_cmd(&self, args: &[String]) -> ExecutionResult {
        let ws = self.current_workspace();
        match self.server_sim.borrow().handle_service_command(args, &ws) {
            Ok(msg) => {
                println!("{msg}");
                ExecutionResult::Builtin
            }
            Err(err) => {
                eprintln!("{err}");
                ExecutionResult::Failed
            }
        }
    }

    fn curl_cmd(&self, args: &[String]) -> ExecutionResult {
        let ws = self.current_workspace();
        match self.server_sim.borrow().handle_curl(args, &ws) {
            Ok(msg) => {
                println!("{msg}");
                ExecutionResult::Builtin
            }
            Err(err) => {
                eprintln!("{err}");
                ExecutionResult::Failed
            }
        }
    }

    fn cadet_cmd(&self, _args: &[String]) -> ExecutionResult {
        let dossier = self.cadet.borrow().render_dossier();
        println!("{dossier}");
        ExecutionResult::Builtin
    }

    fn drill_cmd(&self, args: &[String]) -> ExecutionResult {
        if args.is_empty() || args[0] == "list" {
            let list = self.drills.borrow().list_drills();
            println!("{list}");
            return ExecutionResult::Builtin;
        }

        let ws = match self.ensure_sandbox() {
            Ok(w) => w,
            Err(e) => {
                eprintln!("drill: {e}");
                return ExecutionResult::Failed;
            }
        };

        match args[0].as_str() {
            "start" => {
                if args.len() < 2 {
                    eprintln!("drill: start requires a drill ID (e.g. 'drill start drill-disk')");
                    return ExecutionResult::BuiltinStatus(1);
                }
                if self.interactive.get() {
                    let _ = std::env::set_current_dir(&ws);
                }
                let res = {
                    let mut drills = self.drills.borrow_mut();
                    drills.start_drill(&args[1], &ws)
                };
                match res {
                    Ok(alert) => {
                        println!("{alert}");
                        ExecutionResult::Builtin
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        ExecutionResult::Failed
                    }
                }
            }
            "check" => {
                let (res, active_id) = {
                    let mut drills = self.drills.borrow_mut();
                    let active_id = drills.active_drill_id.clone();
                    let res = drills.check_active(&ws);
                    (res, active_id)
                };
                match res {
                    Ok(msg) => {
                        println!("{msg}");
                        if let Some(id) = active_id {
                            let db = self.database.borrow();
                            self.cadet.borrow_mut().record_drill_completed(&id, db.as_ref());
                        }
                        ExecutionResult::Builtin
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        ExecutionResult::BuiltinStatus(1)
                    }
                }
            }
            "hint" => {
                let hint = {
                    let drills = self.drills.borrow();
                    drills.hint_active()
                };
                if let Some(hint) = hint {
                    println!("{hint}");
                } else {
                    println!("No active drill. Trigger one with 'drill start <id>'.");
                }
                ExecutionResult::Builtin
            }
            unknown => {
                eprintln!("drill: unknown subcommand '{unknown}'. Available: list, start, check, hint");
                ExecutionResult::BuiltinStatus(1)
            }
        }
    }

    fn db_cmd(&self, args: &[String]) -> ExecutionResult {
        let ws = match self.ensure_sandbox() {
            Ok(w) => w,
            Err(e) => {
                eprintln!("db: {e}");
                return ExecutionResult::Failed;
            }
        };

        match crate::server_sim::run_db_command(&ws, args) {
            Ok(output) => {
                println!("{output}");
                self.cadet.borrow_mut().record_db_query(self.database.borrow().as_ref());
                ExecutionResult::Builtin
            }
            Err(err) => {
                eprintln!("db: {err}");
                ExecutionResult::BuiltinStatus(1)
            }
        }
    }

    fn ping_cmd(&self, args: &[String]) -> ExecutionResult {
        let output = crate::server_sim::run_ping_command(args);
        println!("{output}");
        self.cadet.borrow_mut().record_network_probe(self.database.borrow().as_ref());
        ExecutionResult::Builtin
    }

    fn netstat_cmd(&self, _args: &[String]) -> ExecutionResult {
        let ws = self.current_workspace();
        self.cadet.borrow_mut().record_network_probe(self.database.borrow().as_ref());
        println!("\nActive Internet connections (only servers)");
        println!("{:<6} {:<7} {:<7} {:<23} {:<23} {:<10} {:<15}", "Proto", "Recv-Q", "Send-Q", "Local Address", "Foreign Address", "State", "PID/Program name");
        println!("{:<6} {:<7} {:<7} {:<23} {:<23} {:<10} {:<15}", "tcp", "0", "0", "0.0.0.0:22", "0.0.0.0:*", "LISTEN", "842/sshd");
        if self.server_sim.borrow().is_web_running_in(&ws) {
            println!("{:<6} {:<7} {:<7} {:<23} {:<23} {:<10} {:<15}", "tcp", "0", "0", "127.0.0.1:8080", "0.0.0.0:*", "LISTEN", "19820/web.service");
        }
        println!("{:<6} {:<7} {:<7} {:<23} {:<23} {:<10} {:<15}", "tcp", "0", "0", "127.0.0.1:5432", "0.0.0.0:*", "LISTEN", "1042/postgres");
        println!("{:<6} {:<7} {:<7} {:<23} {:<23} {:<10} {:<15}", "tcp", "0", "0", "127.0.0.1:6379", "0.0.0.0:*", "LISTEN", "1105/redis-server");
        println!();
        ExecutionResult::Builtin
    }

    fn tour_cmd(&self, _args: &[String]) -> ExecutionResult {
        let mut out = String::new();
        out.push_str("\n\x1b[1;36m╔════════════════════════════════════════════════════════════════════════════════════╗\x1b[0m\n");
        out.push_str("║                 ✈️  WELCOME TO THE SHELLPILOT FLIGHT SIMULATOR TOUR                 ║\n");
        out.push_str("\x1b[1;36m╚════════════════════════════════════════════════════════════════════════════════════╝\x1b[0m\n\n");
        out.push_str("ShellPilot combines an authentic POSIX shell with a virtual production lab, time-travel,\n");
        out.push_str("embedded database, and structured pipeline tools. Here is what you can explore:\n\n");
        out.push_str("1. \x1b[1;33m🌳 Filesystem & Virtual Staging Server (~/lab)\x1b[0m\n");
        out.push_str("   • \x1b[1mtree\x1b[0m                     : Visual ASCII file hierarchy with color coding\n");
        out.push_str("   • \x1b[1mcat config/server.conf\x1b[0m   : Inspect realistic production service configs\n");
        out.push_str("   • \x1b[1mcheat <command>\x1b[0m          : Instant offline flag recipes (e.g. 'cheat grep')\n\n");
        out.push_str("2. \x1b[1;33m⚙️ Virtual Microservices & Network Testing\x1b[0m\n");
        out.push_str("   • \x1b[1mservice status\x1b[0m           : Inspect mock 'web' and 'worker' daemons\n");
        out.push_str("   • \x1b[1mservice start web\x1b[0m        : Boot the payment gateway microservice\n");
        out.push_str("   • \x1b[1mcurl http://localhost:8080/health\x1b[0m : Test HTTP endpoints\n");
        out.push_str("   • \x1b[1mnetstat\x1b[0m                  : Inspect listening network ports and sockets\n");
        out.push_str("   • \x1b[1mping localhost\x1b[0m           : Measure ICMP latency\n\n");
        out.push_str("3. \x1b[1;33m🗄️ Embedded SQLite Database Engine\x1b[0m\n");
        out.push_str("   • \x1b[1mdb\x1b[0m                       : View active database tables and metrics\n");
        out.push_str("   • \x1b[1mdb \"SELECT * FROM customers WHERE plan = 'enterprise'\"\x1b[0m\n");
        out.push_str("   • \x1b[1mdb schema\x1b[0m                : View relational schema definitions\n\n");
        out.push_str("4. \x1b[1;33m⏳ Time-Travel & Safe Experimentation\x1b[0m\n");
        out.push_str("   • \x1b[1mwhatif 'rm -rf logs/*.log'\x1b[0m: Preview blast radius before running dangerous commands\n");
        out.push_str("   • \x1b[1mundo diff\x1b[0m                : View unified color diff of recent file changes\n");
        out.push_str("   • \x1b[1mundo\x1b[0m                    : Instantly roll back workspace to previous state\n\n");
        out.push_str("5. \x1b[1;33m🔮 Structured Stream Operator (|>)\x1b[0m\n");
        out.push_str("   • \x1b[1mcat data/inventory.jsonl |> .name\x1b[0m : Project JSON fields\n");
        out.push_str("   • \x1b[1mcat data/inventory.jsonl |> take 2\x1b[0m: Take top N structured records\n");
        out.push_str("   • \x1b[1mcat app/config.json |> yaml\x1b[0m        : Transmute JSON to YAML\n\n");
        out.push_str("6. \x1b[1;33m🎓 Academy & Cadet Progression\x1b[0m\n");
        out.push_str("   • \x1b[1mtutor\x1b[0m                    : Browse 26 lessons across 7 curriculum tracks\n");
        out.push_str("   • \x1b[1mdrill\x1b[0m                    : Face 7 high-pressure chaos outage scenarios\n");
        out.push_str("   • \x1b[1mcadet\x1b[0m                    : View your rank, XP, and 11 unlockable badges\n\n");
        println!("{out}");
        ExecutionResult::Builtin
    }

    fn snapshot_cmd(&self, args: &[String]) -> ExecutionResult {
        let _ = self.ensure_sandbox();
        let label = if args.is_empty() {
            "manual snapshot".to_string()
        } else {
            args.join(" ")
        };
        let mut sb_borrow = self.sandbox.borrow_mut();
        if let Some(ref mut sb) = *sb_borrow {
            match sb.pre_command_snapshot(&label) {
                Ok(id) => {
                    println!("📸 Saved checkpoint #{id}: '{label}'");
                    ExecutionResult::Builtin
                }
                Err(err) => {
                    eprintln!("snapshot: {err}");
                    ExecutionResult::Failed
                }
            }
        } else {
            eprintln!("snapshot: failed to access sandbox");
            ExecutionResult::Failed
        }
    }

    pub fn execute(&self, command: &ParsedCommand) -> ExecutionResult {
        if builtins::is_builtin(&command.program) {
            let _environment = match ScopedEnvironment::apply(&command.environment) {
                Ok(environment) => environment,
                Err(error) => {
                    eprintln!("{}: {error}", command.program);
                    return ExecutionResult::Failed;
                }
            };
            let _redirections = match BuiltinRedirections::apply(&command.redirects) {
                Ok(redirections) => redirections,
                Err(error) => {
                    eprintln!("{}: {error}", command.program);
                    return ExecutionResult::Failed;
                }
            };
            match command.program.as_str() {
                "alias" => return self.alias(&command.args),
                "unalias" => return self.unalias(&command.args),
                "type" => return self.type_command(&command.args),
                "unset" => return self.unset(&command.args),
                "jobs" => return self.list_jobs(&command.args),
                "wait" => return self.wait_jobs(&command.args),
                "kill" => return self.kill_job(&command.args),
                "fg" => return self.foreground_job(&command.args),
                "bg" => return self.background_job(&command.args),
                "history" => return self.list_history(&command.args),
                "timeline" => return self.list_timeline(&command.args),
                "explain" => return self.explain(&command.args),
                "policy" => return self.policy(&command.args),
                "abbr" => return self.abbr(&command.args),
                "processes" => return list_processes(&command.args),
                "network" => return list_network(&command.args),
                "ports" => return list_ports(&command.args),
                "connections" => return list_connections(&command.args),
                "children" => return list_children(&command.args),
                "json" => return format_json(&command.args),
                "yaml" => return format_yaml(&command.args),
                "toml" => return format_toml(&command.args),
                "help" => return show_help(&command.args),
                "tutor" => return self.tutor_cmd(&command.args),
                "whatif" => return self.whatif_cmd(&command.args),
                "undo" => return self.undo_cmd(&command.args),
                "snapshot" => return self.snapshot_cmd(&command.args),
                "tree" => return self.tree_cmd(&command.args),
                "cheat" => return self.cheat_cmd(&command.args),
                "doctor" => return self.doctor_cmd(&command.args),
                "service" => return self.service_cmd(&command.args),
                "curl" => return self.curl_cmd(&command.args),
                "cadet" | "profile" => return self.cadet_cmd(&command.args),
                "drill" => return self.drill_cmd(&command.args),
                "db" => return self.db_cmd(&command.args),
                "ping" => return self.ping_cmd(&command.args),
                "netstat" => return self.netstat_cmd(&command.args),
                "tour" | "explore" => return self.tour_cmd(&command.args),
                "command" => {
                    return match builtins::execute(
                        &command.program,
                        &command.args,
                        find_executable_in_path,
                    ) {
                        Ok(BuiltinResult::Status(status)) => ExecutionResult::BuiltinStatus(status),
                        Ok(BuiltinResult::Handled) => ExecutionResult::Builtin,
                        Ok(BuiltinResult::Exit(status)) => ExecutionResult::Exit(status),
                        Err(error) => {
                            eprintln!("{error}");
                            ExecutionResult::Failed
                        }
                    };
                }
                _ => {}
            }
            return match builtins::execute(&command.program, &command.args, find_executable_in_path)
            {
                Ok(BuiltinResult::Handled) => ExecutionResult::Builtin,

                Ok(BuiltinResult::Status(status)) => ExecutionResult::BuiltinStatus(status),

                Ok(BuiltinResult::Exit(status)) => ExecutionResult::Exit(status),

                Err(error) => {
                    eprintln!("{error}");
                    ExecutionResult::Failed
                }
            };
        }

        self.execute_external(command).0
    }

    pub fn execute_ast(&self, ast: &Ast) -> ExecutionResult {
        self.record_history_ast(ast);
        match ast {
            Ast::Pipeline(pipeline) => self.execute_pipeline(pipeline),
            Ast::If(if_stmt) => self.execute_if(if_stmt),
            Ast::For(parsed_for) => self.execute_for(parsed_for),
            Ast::While(parsed_while) => self.execute_while(parsed_while),
            Ast::Function(func) => self.define_function(func),
            Ast::Sequence(sequence) => self.execute_sequence(sequence),
            Ast::StructuredPipeline(sp) => self.execute_structured_pipeline(sp),
        }
    }

    pub fn execute_sequence(&self, sequence: &AstSequence) -> ExecutionResult {
        let mut last_result = ExecutionResult::Builtin;

        for item in &sequence.items {
            if let Some(ref op) = item.op {
                let prev_success = last_result.status_code() == 0;
                match op {
                    LogicalOp::And => {
                        if !prev_success {
                            continue;
                        }
                    }
                    LogicalOp::Or => {
                        if prev_success {
                            continue;
                        }
                    }
                }
            }

            let node = if !item.raw_tokens.is_empty() {
                crate::parser::parse_ast_from_tokens(&item.raw_tokens, last_result.status_code())
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| item.node.clone())
            } else {
                item.node.clone()
            };

            last_result = match &node {
                Ast::Pipeline(pipeline) => {
                    if item.background {
                        let mut p = pipeline.clone();
                        p.background = true;
                        self.execute_pipeline(&p)
                    } else {
                        self.execute_pipeline(pipeline)
                    }
                }
                Ast::If(if_stmt) => self.execute_if(if_stmt),
                Ast::For(parsed_for) => self.execute_for(parsed_for),
                Ast::While(parsed_while) => self.execute_while(parsed_while),
                Ast::Function(func) => self.define_function(func),
                Ast::Sequence(seq) => self.execute_sequence(seq),
                Ast::StructuredPipeline(sp) => self.execute_structured_pipeline(sp),
            };

            if matches!(last_result, ExecutionResult::Exit(_)) {
                break;
            }
        }

        last_result
    }

    pub fn execute_if(&self, if_stmt: &ParsedIf) -> ExecutionResult {
        let condition_result = self.execute_pipeline(&if_stmt.condition);
        if condition_result.status_code() == 0 {
            for ast in &if_stmt.then_branch {
                let _ = self.execute_ast(ast);
            }
        } else if let Some(else_branch) = &if_stmt.else_branch {
            for ast in else_branch {
                let _ = self.execute_ast(ast);
            }
        }
        ExecutionResult::Builtin
    }

    pub fn execute_for(&self, parsed_for: &ParsedFor) -> ExecutionResult {
        let mut last_res = ExecutionResult::BuiltinStatus(0);
        for value in &parsed_for.values {
            let expanded_values = crate::parser::expand_tokens(vec![value.clone()], last_res.status_code());
            for expanded_value in expanded_values {
                unsafe { std::env::set_var(&parsed_for.variable, &expanded_value) };
                if let Ok(body) = parsed_for.parse_body(last_res.status_code()) {
                    for statement in &body {
                        last_res = self.execute_ast(statement);
                        if matches!(last_res, ExecutionResult::Failed | ExecutionResult::Exit(_)) {
                            break;
                        }
                    }
                }
            }
        }
        last_res
    }

    pub fn execute_while(&self, parsed_while: &ParsedWhile) -> ExecutionResult {
        let mut last_res = ExecutionResult::BuiltinStatus(0);
        loop {
            let mut cond_success = true;
            let condition = match parsed_while.parse_condition(last_res.status_code()) {
                Ok(cond) => cond,
                Err(_) => break,
            };
            for stmt in &condition {
                let res = self.execute_ast(stmt);
                if res.status_code() != 0 {
                    cond_success = false;
                    break;
                }
            }
            if !cond_success {
                break;
            }
            let body = match parsed_while.parse_body(last_res.status_code()) {
                Ok(b) => b,
                Err(_) => break,
            };
            for statement in &body {
                last_res = self.execute_ast(statement);
                if matches!(last_res, ExecutionResult::Failed | ExecutionResult::Exit(_)) {
                    return last_res;
                }
            }
        }
        last_res
    }

    pub fn define_function(&self, func: &ParsedFunction) -> ExecutionResult {
        self.functions.borrow_mut().insert(func.name.clone(), func.clone());
        ExecutionResult::BuiltinStatus(0)
    }

    pub fn execute_function_call(&self, func: &ParsedFunction, command: &ParsedCommand) -> ExecutionResult {
        let old_params = crate::parser::get_positional_params();
        let mut new_params = vec![command.program.clone()];
        new_params.extend(command.args.clone());
        crate::parser::set_positional_params(new_params);

        let _redirs = match BuiltinRedirections::apply(&command.redirects) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{}: {e}", command.program);
                crate::parser::set_positional_params(old_params);
                return ExecutionResult::Failed;
            }
        };

        for (name, val) in &command.environment {
            unsafe { env::set_var(name, val); }
        }

        let body = match func.parse_body(0) {
            Ok(b) => b,
            Err(err) => {
                eprintln!("shellpilot: {}: {err}", func.name);
                crate::parser::set_positional_params(old_params);
                return ExecutionResult::Failed;
            }
        };

        let mut last_res = ExecutionResult::BuiltinStatus(0);
        for stmt in &body {
            last_res = self.execute_ast(stmt);
            if matches!(last_res, ExecutionResult::Failed | ExecutionResult::Exit(_)) {
                break;
            }
        }

        crate::parser::set_positional_params(old_params);
        last_res
    }

    fn execute_structured_pipeline(
        &self,
        sp: &crate::parser::ParsedStructuredPipeline,
    ) -> ExecutionResult {
        let (output, exit_code) = match self.execute_capture_ast(&sp.source) {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!("shellpilot: {err}");
                return ExecutionResult::Failed;
            }
        };

        if exit_code != 0 && output.trim().is_empty() {
            return ExecutionResult::BuiltinStatus(exit_code);
        }

        let mut current = match parse_structured_data(&output) {
            Ok(val) => val,
            Err(err) => {
                eprintln!("shellpilot: failed to parse structured data: {err}");
                return ExecutionResult::Failed;
            }
        };

        let mut explicit_format = None;
        for stage in &sp.stages {
            match stage {
                crate::parser::StructuredOp::Project(path) => {
                    current = project_structured_value(&current, path);
                }
                crate::parser::StructuredOp::Filter(expr) => {
                    current = filter_structured_value(&current, expr);
                }
                crate::parser::StructuredOp::Take(n) => {
                    if let serde_json::Value::Array(arr) = current {
                        current = serde_json::Value::Array(arr.into_iter().take(*n).collect());
                    }
                }
                crate::parser::StructuredOp::Skip(n) => {
                    if let serde_json::Value::Array(arr) = current {
                        current = serde_json::Value::Array(arr.into_iter().skip(*n).collect());
                    }
                }
                crate::parser::StructuredOp::Count => {
                    let count = match &current {
                        serde_json::Value::Array(arr) => arr.len(),
                        serde_json::Value::Object(map) => map.len(),
                        serde_json::Value::Null => 0,
                        _ => 1,
                    };
                    current = serde_json::json!(count);
                }
                crate::parser::StructuredOp::Format(fmt) => {
                    explicit_format = Some(fmt.clone());
                }
            }
        }

        render_structured_output(&current, explicit_format);
        ExecutionResult::BuiltinStatus(0)
    }

    pub fn execute_capture_ast(&self, ast: &Ast) -> Result<(String, i32), String> {
        let mut fds = [0; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
            return Err(std::io::Error::last_os_error().to_string());
        }

        let pid = unsafe { libc::fork() };
        if pid < 0 {
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return Err(std::io::Error::last_os_error().to_string());
        }

        if pid == 0 {
            unsafe {
                libc::close(fds[0]);
                libc::dup2(fds[1], libc::STDOUT_FILENO);
                libc::close(fds[1]);
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::signal(libc::SIGQUIT, libc::SIG_DFL);
                libc::signal(libc::SIGTSTP, libc::SIG_DFL);
            }
            let executor = Executor::new();
            let result = executor.execute_ast(ast);
            unsafe { libc::_exit(result.status_code()); }
        }

        unsafe { libc::close(fds[1]); }
        let mut file = unsafe { File::from_raw_fd(fds[0]) };
        let mut output = String::new();
        use std::io::Read;
        let _ = file.read_to_string(&mut output);
        let mut status = 0;
        unsafe { libc::waitpid(pid, &mut status, 0); }
        let exit_code = if libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status)
        } else {
            1
        };
        Ok((output, exit_code))
    }

    pub fn execute_pipeline(&self, pipeline: &ParsedPipeline) -> ExecutionResult {
        let started = Instant::now();
        let working_directory = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        // self.record_history(pipeline); // Moved to execute_ast
        let effects = analyze_pipeline(pipeline);
        let decision = policy::evaluate(&effects);

        let denied = if decision == PolicyDecision::Block {
            eprintln!("shellpilot: command blocked by policy ({decision})");
            true
        } else if decision == PolicyDecision::Ask && !self.confirm_execution(pipeline, &effects) {
            eprintln!("shellpilot: command denied by policy");
            true
        } else {
            false
        };

        let modifies = effects.iter().any(|e| {
            matches!(
                e,
                crate::effects::Effect::FilesystemDelete(_)
                    | crate::effects::Effect::FilesystemWrite(_)
                    | crate::effects::Effect::SensitivePathWrite(_)
            )
        });
        if !denied && modifies
            && let Some(ref mut sb) = *self.sandbox.borrow_mut() {
                let cmd_str = pipeline
                    .commands
                    .iter()
                    .map(format_command)
                    .collect::<Vec<_>>()
                    .join(" | ");
                sb.pre_command_snapshot(&cmd_str).ok();
            }

        let (result, stages) = if denied {
            (ExecutionResult::Failed, Vec::new())
        } else if pipeline.background {
            let (result, stages) = self.start_background_pipeline(pipeline);
            (result, stages)
        } else if pipeline.commands.len() == 1 {
            let mut command = pipeline.commands[0].clone();
            if let Some(alias_val) = self.aliases.borrow().get(&command.program) {
                let parts: Vec<String> = alias_val.split_whitespace().map(|s| s.to_string()).collect();
                if !parts.is_empty() {
                    command.program = parts[0].clone();
                    let mut new_args = parts[1..].to_vec();
                    new_args.extend(command.args);
                    command.args = new_args;
                }
            }

            let (result, process_id) = if let Some(func) = self.functions.borrow().get(&command.program).cloned() {
                (self.execute_function_call(&func, &command), None)
            } else if builtins::is_builtin(&command.program) {
                if command.program == "env"
                    && command
                        .args
                        .iter()
                        .any(|argument| argument.contains('='))
                {
                    (self.execute_env_command(&command), None)
                } else {
                    (self.execute(&command), None)
                }
            } else {
                self.execute_external(&command)
            };
            let stages = if result.status_code() >= 0 {
                vec![ProcessRecord {
                    program: command.program.clone(),
                    process_id,
                }]
            } else {
                Vec::new()
            };
            (result, stages)
        } else {
            self.execute_external_pipeline(pipeline)
        };

        let audit = AuditEvent {
            decision,
            outcome: if decision == PolicyDecision::Block {
                AuditOutcome::Blocked
            } else if denied {
                AuditOutcome::Denied
            } else {
                AuditOutcome::Executed
            },
        };
        let record = ExecutionRecord {
            command: pipeline
                .commands
                .iter()
                .map(format_command)
                .collect::<Vec<_>>()
                .join(" | "),
            working_directory,
            duration_ms: started.elapsed().as_millis(),
            status: result.status_code(),
            environment_keys: effective_environment_keys(pipeline),
            redirections: pipeline
                .commands
                .iter()
                .flat_map(|command| command.redirects.iter().cloned())
                .collect(),
            stages,
            policy: decision,
            risk: policy::risk_level(&effects),
            audit,
            effects,
        };
        if let Some(database) = self.database.borrow().as_ref()
            && let Err(error) = database.execute(
                "INSERT INTO executions
                 (command, working_directory, duration_ms, status, policy, audit, effects, metadata, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now'))",
                params![
                    record.command,
                    record.working_directory.to_string_lossy(),
                    record.duration_ms as i64,
                    record.status,
                    record.policy.to_string(),
                    format!("{:?}", record.audit.outcome),
                    format!("{:?}", record.effects),
                    timeline_metadata(&record).to_string(),
                ],
            ) {
                eprintln!("timeline: failed to persist execution: {error}");
            }
        self.timeline.borrow_mut().push(record);
        result
    }

    fn execute_env_command(&self, command: &ParsedCommand) -> ExecutionResult {
        let mut assignments = command.environment.clone();
        let mut command_index = 0;
        while command_index < command.args.len() {
            let argument = &command.args[command_index];
            let Some((name, value)) = argument.split_once('=') else {
                break;
            };
            if !is_valid_environment_name(name) {
                eprintln!("env: invalid variable name: {name}");
                return ExecutionResult::Failed;
            }
            assignments.push((name.to_owned(), value.to_owned()));
            command_index += 1;
        }

        if command_index == command.args.len() {
            let listing = ParsedCommand {
                program: "env".to_owned(),
                args: Vec::new(),
                environment: assignments,
                redirects: command.redirects.clone(),
            };
            return self.execute(&listing);
        }

        let target = ParsedCommand {
            program: command.args[command_index].clone(),
            args: command.args[command_index + 1..].to_vec(),
            environment: assignments,
            redirects: command.redirects.clone(),
        };
        self.execute(&target)
    }

    fn confirm_execution(&self, pipeline: &ParsedPipeline, effects: &[Effect]) -> bool {
        if let Some(ref sb) = *self.sandbox.borrow() {
            let all_jailed = effects.iter().all(|e| match e {
                Effect::FilesystemWrite(p) | Effect::FilesystemDelete(p) => {
                    sb.is_path_jailed(std::path::Path::new(p), true)
                }
                _ => true,
            });
            if all_jailed {
                return true;
            }
        }

        if !self.interactive.get() || unsafe { libc::isatty(libc::STDIN_FILENO) != 1 } {
            return false;
        }

        let command = pipeline
            .commands
            .iter()
            .map(format_command)
            .collect::<Vec<_>>()
            .join(" | ");

        let mut blast_summaries = Vec::new();
        for effect in effects {
            if let Effect::FilesystemDelete(path) = effect {
                if let Some(radius) = crate::effects::calculate_blast_radius_for_path(path) {
                    blast_summaries.push(format!(
                        "This will delete {} file(s) ({}) in '{}'",
                        radius.file_count,
                        crate::effects::format_bytes(radius.total_bytes),
                        radius.path
                    ));
                }
            } else if let Effect::SensitivePathWrite(path) = effect
                && let Some(radius) = crate::effects::calculate_blast_radius_for_path(path) {
                    blast_summaries.push(format!(
                        "This will modify/overwrite {} file(s) ({}) in sensitive path '{}'",
                        radius.file_count,
                        crate::effects::format_bytes(radius.total_bytes),
                        radius.path
                    ));
                }
        }

        if !blast_summaries.is_empty() {
            for summary in &blast_summaries {
                eprintln!("shellpilot: ⚠️ Blast-radius preview: {summary}");
            }
        } else {
            eprintln!("shellpilot: policy asks before executing: {command}");
            eprintln!("effects: {effects:?}");
        }
        eprint!("Confirm execution? [y/N] ");
        use std::io::Write;
        std::io::stderr().flush().ok();

        let mut response = String::new();
        if std::io::stdin().read_line(&mut response).is_err() {
            return false;
        }
        matches!(response.trim(), "y" | "Y" | "yes" | "YES")
    }

    fn execute_external_pipeline(
        &self,
        pipeline: &ParsedPipeline,
    ) -> (ExecutionResult, Vec<ProcessRecord>) {
        let children = match spawn_pipeline_processes(pipeline) {
            Ok(children) => children,
            Err(SpawnError::CommandNotFound) => {
                return (ExecutionResult::CommandNotFound, Vec::new());
            }
            Err(SpawnError::Failed) => return (ExecutionResult::Failed, Vec::new()),
        };

        let process_ids = children.iter().map(ProcessHandle::id).collect::<Vec<_>>();
        let process_group = process_ids.first().copied();
        let statuses = run_in_foreground_group(process_group, || wait_for_pipeline(children));

        match statuses {
            Ok(PipelineWaitOutcome::Completed(statuses)) => (
                ExecutionResult::Pipeline(statuses),
                pipeline
                    .commands
                    .iter()
                    .zip(process_ids)
                    .map(|(command, process_id)| ProcessRecord {
                        program: command.program.clone(),
                        process_id: Some(process_id),
                    })
                    .collect(),
            ),
            Ok(PipelineWaitOutcome::Stopped { children, statuses }) => {
                println!(
                    "\n[stopped] {}",
                    pipeline
                        .commands
                        .iter()
                        .map(format_command)
                        .collect::<Vec<_>>()
                        .join(" | ")
                );
                let mut process_ids = Vec::with_capacity(children.len());
                for child in &children {
                    process_ids.push(child.id());
                }
                let recorded_process_ids = process_ids.clone();
                if !children.is_empty() {
                    self.jobs.borrow_mut().push(Job {
                        id: next_job_id(&self.next_job_id),
                        command: pipeline
                            .commands
                            .iter()
                            .map(format_command)
                            .collect::<Vec<_>>()
                            .join(" | "),
                        children,
                        process_ids,
                        state: JobState::Stopped,
                    });
                }
                let _ = statuses;
                (
                    ExecutionResult::Failed,
                    pipeline
                        .commands
                        .iter()
                        .zip(recorded_process_ids)
                        .map(|(command, process_id)| ProcessRecord {
                            program: command.program.clone(),
                            process_id: Some(process_id),
                        })
                        .collect(),
                )
            }
            Err(error) => {
                eprintln!("shellpilot: failed waiting for pipeline: {error}");
                (ExecutionResult::Failed, Vec::new())
            }
        }
    }

    fn execute_external(&self, command: &ParsedCommand) -> (ExecutionResult, Option<u32>) {
        let executable = match find_executable_in_path(&command.program) {
            Some(path) => path,

            None => {
                eprintln!("{}: command not found", command.program);
                return (ExecutionResult::CommandNotFound, None);
            }
        };

        let mut process = Command::new(&executable);
        process.args(&command.args);
        process.envs(
            command
                .environment
                .iter()
                .map(|(name, value)| (name, value)),
        );
        reset_child_signals(&mut process, None);
        if let Err(error) = apply_redirections(&mut process, &command.redirects) {
            eprintln!("{}: {error}", command.program);
            return (ExecutionResult::Failed, None);
        }

        let child = match process.spawn() {
            Ok(child) => child,
            Err(error) => {
                eprintln!("{}: failed to execute: {}", command.program, error);
                return (ExecutionResult::Failed, None);
            }
        };
        let process_id = child.id();
        match run_in_foreground_group(Some(process_id), || wait_for_process(process_id)) {
            Ok(WaitOutcome::Exited(status)) => {
                (ExecutionResult::External(status), Some(process_id))
            }
            Ok(WaitOutcome::Stopped) => {
                println!("\n[stopped] {}", format_command(command));
                self.jobs.borrow_mut().push(Job {
                    id: next_job_id(&self.next_job_id),
                    command: format_command(command),
                    children: vec![ProcessHandle::External(child)],
                    process_ids: vec![process_id],
                    state: JobState::Stopped,
                });
                (ExecutionResult::Failed, Some(process_id))
            }
            Err(error) => {
                eprintln!("{}: failed waiting for process: {}", command.program, error);
                (ExecutionResult::Failed, Some(process_id))
            }
        }
    }

    fn start_background(&self, command: &ParsedCommand) -> ExecutionResult {
        if builtins::is_builtin(&command.program) {
            let pid = unsafe { libc::fork() };
            if pid < 0 {
                eprintln!("{}: failed to fork background builtin", command.program);
                return ExecutionResult::Failed;
            }
            if pid == 0 {
                unsafe {
                    libc::setpgid(0, 0);
                    libc::signal(libc::SIGINT, libc::SIG_DFL);
                    libc::signal(libc::SIGQUIT, libc::SIG_DFL);
                    libc::signal(libc::SIGTSTP, libc::SIG_DFL);
                }
                for (name, value) in &command.environment {
                    unsafe { env::set_var(name, value); }
                }
                let _redirections = match BuiltinRedirections::apply(&command.redirects) {
                    Ok(r) => r,
                    Err(err) => {
                        eprintln!("{}: {err}", command.program);
                        unsafe { libc::_exit(1); }
                    }
                };
                let res = builtins::execute(&command.program, &command.args, find_executable_in_path);
                use std::io::Write;
                let _ = std::io::stdout().flush();
                let _ = std::io::stderr().flush();
                let code = match res {
                    Ok(builtins::BuiltinResult::Handled) => 0,
                    Ok(builtins::BuiltinResult::Status(s)) => s,
                    Ok(builtins::BuiltinResult::Exit(s)) => s,
                    Err(e) => {
                        eprintln!("{e}");
                        1
                    }
                };
                unsafe { libc::_exit(code); }
            }
            let child_pid = pid as u32;
            let id = *self.next_job_id.borrow();
            *self.next_job_id.borrow_mut() += 1;
            println!("[{id}] {child_pid}");
            self.jobs.borrow_mut().push(Job {
                id,
                command: format_command(command),
                children: vec![ProcessHandle::Forked(child_pid)],
                process_ids: vec![child_pid],
                state: JobState::Running,
            });
            return ExecutionResult::Background;
        }

        let executable = match find_executable_in_path(&command.program) {
            Some(path) => path,
            None => {
                eprintln!("{}: command not found", command.program);
                return ExecutionResult::CommandNotFound;
            }
        };
        let mut process = Command::new(executable);
        process.args(&command.args);
        process.envs(
            command
                .environment
                .iter()
                .map(|(name, value)| (name, value)),
        );
        reset_child_signals(&mut process, None);
        if let Err(error) = apply_redirections(&mut process, &command.redirects) {
            eprintln!("{}: {error}", command.program);
            return ExecutionResult::Failed;
        }
        let child = match process.spawn() {
            Ok(child) => child,
            Err(error) => {
                eprintln!("{}: failed to execute: {error}", command.program);
                return ExecutionResult::Failed;
            }
        };
        let process_id = child.id();
        unsafe {
            let _ = libc::setpgid(process_id as libc::pid_t, process_id as libc::pid_t);
        }
        let id = *self.next_job_id.borrow();
        *self.next_job_id.borrow_mut() += 1;
        println!("[{id}] {}", child.id());
        self.jobs.borrow_mut().push(Job {
            id,
            command: format_command(command),
            children: vec![ProcessHandle::External(child)],
            process_ids: vec![process_id],
            state: JobState::Running,
        });
        ExecutionResult::Background
    }

    fn start_background_pipeline(
        &self,
        pipeline: &ParsedPipeline,
    ) -> (ExecutionResult, Vec<ProcessRecord>) {
        if pipeline.commands.len() == 1 {
            let command = &pipeline.commands[0];
            let result = self.start_background(command);
            let stages = self
                .jobs
                .borrow()
                .last()
                .filter(|_| matches!(&result, ExecutionResult::Background))
                .map(|job| {
                    vec![ProcessRecord {
                        program: command.program.clone(),
                        process_id: job.process_ids.first().copied(),
                    }]
                })
                .unwrap_or_default();
            return (result, stages);
        }
        let children = match spawn_pipeline_processes(pipeline) {
            Ok(children) => children,
            Err(SpawnError::CommandNotFound) => {
                return (ExecutionResult::CommandNotFound, Vec::new());
            }
            Err(SpawnError::Failed) => return (ExecutionResult::Failed, Vec::new()),
        };
        let process_ids = children.iter().map(ProcessHandle::id).collect::<Vec<_>>();
        let id = next_job_id(&self.next_job_id);
        println!("[{id}] {}", process_ids[0]);
        self.jobs.borrow_mut().push(Job {
            id,
            command: pipeline
                .commands
                .iter()
                .map(format_command)
                .collect::<Vec<_>>()
                .join(" | "),
            children,
            process_ids: process_ids.clone(),
            state: JobState::Running,
        });
        let stages = pipeline
            .commands
            .iter()
            .zip(process_ids)
            .map(|(command, process_id)| ProcessRecord {
                program: command.program.clone(),
                process_id: Some(process_id),
            })
            .collect();
        (ExecutionResult::Background, stages)
    }

    fn list_jobs(&self, args: &[String]) -> ExecutionResult {
        if !args.is_empty() {
            eprintln!("jobs: too many arguments");
            return ExecutionResult::Failed;
        }
        let mut jobs = self.jobs.borrow_mut();
        let mut active = Vec::new();
        for mut job in jobs.drain(..) {
            if job.state == JobState::Running
                && job.process_ids.iter().any(|pid| is_process_stopped(*pid))
            {
                job.state = JobState::Stopped;
            }
            let mut statuses = Vec::new();
            let mut running = false;
            for child in &mut job.children {
                match child.try_wait() {
                    Ok(Some(status)) => statuses.push(status),
                    Ok(None) => running = true,
                    Err(error) => {
                        eprintln!("[{}] {}: {error}", job.id, job.command);
                        running = true;
                    }
                }
            }
            if !running {
                let status = statuses.last().copied().expect("job has children");
                println!("[{}] Done ({}) {}", job.id, status, job.command);
            } else {
                let state = match job.state {
                    JobState::Running => "Running",
                    JobState::Stopped => "Stopped",
                };
                println!("[{}] {} {}", job.id, state, job.command);
                active.push(job);
            }
        }
        *jobs = active;
        ExecutionResult::Builtin
    }

    fn wait_jobs(&self, args: &[String]) -> ExecutionResult {
        let requested = match args {
            [] => None,
            [_value] => match parse_job_id(args, "wait") {
                Ok(id) => Some(id),
                Err(error) => {
                    eprintln!("{error}");
                    return ExecutionResult::Failed;
                }
            },
            _ => {
                eprintln!("wait: too many arguments");
                return ExecutionResult::Failed;
            }
        };
        let mut jobs = self.jobs.borrow_mut();
        let mut remaining = Vec::new();
        let mut result = ExecutionResult::Builtin;
        let mut found = false;
        for mut job in jobs.drain(..) {
            if requested.is_some() && requested != Some(job.id) {
                remaining.push(job);
                continue;
            }
            found = true;
            if job.state == JobState::Stopped
                && let Err(error) = continue_job(&job.process_ids) {
                    eprintln!("wait: job {}: {error}", job.id);
                    remaining.push(job);
                    return ExecutionResult::Failed;
                }
            let mut statuses = Vec::new();
            let mut failed = false;
            for child in &mut job.children {
                match child.wait() {
                    Ok(status) => statuses.push(status),
                    Err(error) => {
                        eprintln!("wait: job {}: {error}", job.id);
                        failed = true;
                        break;
                    }
                }
            }
            if failed {
                remaining.push(job);
                continue;
            }
            if let Some(status) = statuses.last() {
                result = if statuses.len() == 1 {
                    ExecutionResult::External(*status)
                } else {
                    ExecutionResult::Pipeline(statuses)
                };
            }
        }
        if requested.is_some() && !found {
            eprintln!("wait: job not found");
            *jobs = remaining;
            return ExecutionResult::Failed;
        }
        if let Some(id) = requested
            && !remaining.iter().any(|job| job.id == id) && result.status_code() == 0 {
                println!("[{id}] Done");
            }
        *jobs = remaining;
        result
    }

    fn kill_job(&self, args: &[String]) -> ExecutionResult {
        let id = match parse_job_id(args, "kill") {
            Ok(id) => id,
            Err(error) => {
                eprintln!("{error}");
                return ExecutionResult::Failed;
            }
        };
        let jobs = self.jobs.borrow();
        let Some(job) = jobs.iter().find(|job| job.id == id) else {
            eprintln!("kill: job {id} not found");
            return ExecutionResult::Failed;
        };
        let pid = job.process_ids[0] as libc::pid_t;
        let process_group = -pid;
        if unsafe { libc::kill(process_group, libc::SIGTERM) } == -1
            && unsafe { libc::kill(pid, libc::SIGTERM) } == -1 {
                eprintln!("kill: job {id}: {}", std::io::Error::last_os_error());
                return ExecutionResult::Failed;
            }
        println!("[{id}] Terminated {}", job.command);
        ExecutionResult::Builtin
    }

    fn record_history_ast(&self, ast: &Ast) {
        let command = format_ast(ast);
        self.history.borrow_mut().push(command.clone());
        if let Some(database) = self.database.borrow().as_ref()
            && let Err(error) = database.execute(
                "INSERT INTO history (command, created_at) VALUES (?1, datetime('now'))",
                params![command],
            ) {
                eprintln!("history: failed to persist command: {error}");
            }
    }

    #[allow(dead_code)]
    fn record_history(&self, pipeline: &ParsedPipeline) {
        let command = pipeline
            .commands
            .iter()
            .map(format_command)
            .collect::<Vec<_>>()
            .join(" | ");
        self.history.borrow_mut().push(command.clone());
        if let Some(database) = self.database.borrow().as_ref()
            && let Err(error) = database.execute(
                "INSERT INTO history (command, created_at) VALUES (?1, datetime('now'))",
                params![command],
            ) {
                eprintln!("history: failed to persist command: {error}");
            }
    }

    fn list_history(&self, args: &[String]) -> ExecutionResult {
        let mut history = self.history.borrow_mut();
        match args {
            [] => {}
            [command, term] if command == "search" => {
                drop(history);
                return self.search_history(term);
            }
            [cmd, path @ ..] if cmd == "dir" || cmd == "--dir" => {
                drop(history);
                let target_dir = if let Some(p) = path.first() {
                    PathBuf::from(p)
                } else {
                    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                };
                return self.filter_history_by_dir(&target_dir);
            }
            [cmd, status @ ..] if cmd == "status" || cmd == "exit" || cmd == "--status" || cmd == "--exit" || cmd == "--failed" => {
                drop(history);
                let filter_failed = cmd == "--failed";
                let status_code = if filter_failed {
                    None
                } else if let Some(s) = status.first() {
                    s.parse::<i32>().ok()
                } else {
                    None
                };
                return self.filter_history_by_status(status_code, filter_failed);
            }
            [command] if command == "clear" => {
                history.clear();
                if let Some(database) = self.database.borrow().as_ref()
                    && let Err(error) = database.execute("DELETE FROM history", []) {
                        eprintln!("history: failed to clear persistent history: {error}");
                        return ExecutionResult::Failed;
                    }
                return ExecutionResult::Builtin;
            }
            [limit] => {
                let limit = match limit.parse::<usize>() {
                    Ok(limit) => limit,
                    Err(_) => {
                        eprintln!("history: expected a count or 'clear'");
                        return ExecutionResult::Failed;
                    }
                };
                let start = history.len().saturating_sub(limit);
                for (index, command) in history.iter().enumerate().skip(start) {
                    println!("{:>5}  {command}", index + 1);
                }
                return ExecutionResult::Builtin;
            }
            _ => {
                eprintln!("history: too many arguments");
                return ExecutionResult::Failed;
            }
        }

        for (index, command) in history.iter().enumerate() {
            println!("{:>5}  {command}", index + 1);
        }
        ExecutionResult::Builtin
    }

    fn search_history(&self, term: &str) -> ExecutionResult {
        let database_ref = self.database.borrow();
        let Some(database) = database_ref.as_ref() else {
            for (index, command) in self.history.borrow().iter().enumerate() {
                if command.contains(term) {
                    println!("{:>5}  {command}", index + 1);
                }
            }
            return ExecutionResult::Builtin;
        };
        let mut statement = match database
            .prepare("SELECT rowid, command FROM history WHERE command LIKE ?1 ORDER BY rowid")
        {
            Ok(statement) => statement,
            Err(error) => {
                eprintln!("history: search failed: {error}");
                return ExecutionResult::Failed;
            }
        };
        let pattern = format!("%{term}%");
        let rows = match statement.query_map(params![pattern], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        }) {
            Ok(rows) => rows,
            Err(error) => {
                eprintln!("history: search failed: {error}");
                return ExecutionResult::Failed;
            }
        };
        for row in rows {
            match row {
                Ok((id, command)) => println!("{id:>5}  {command}"),
                Err(error) => eprintln!("history: search row failed: {error}"),
            }
        }
        ExecutionResult::Builtin
    }

    fn search_timeline(&self, term: &str) -> ExecutionResult {
        let database_ref = self.database.borrow();
        let Some(database) = database_ref.as_ref() else {
            eprintln!("timeline: persistent history is unavailable");
            return ExecutionResult::Failed;
        };
        let mut statement = match database.prepare(
            "SELECT rowid, command, status, policy, audit
             FROM executions WHERE command LIKE ?1 ORDER BY rowid",
        ) {
            Ok(statement) => statement,
            Err(error) => {
                eprintln!("timeline: search failed: {error}");
                return ExecutionResult::Failed;
            }
        };
        let pattern = format!("%{term}%");
        let rows = match statement.query_map(params![pattern], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i32>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        }) {
            Ok(rows) => rows,
            Err(error) => {
                eprintln!("timeline: search failed: {error}");
                return ExecutionResult::Failed;
            }
        };
        for row in rows {
            match row {
                Ok((id, command, status, policy, audit)) => {
                    println!("{id:>5}  [{status}] {command}  policy={policy} audit={audit}")
                }
                Err(error) => eprintln!("timeline: search row failed: {error}"),
            }
        }
        ExecutionResult::Builtin
    }

    fn load_history(&self, database: &Connection) {
        let mut statement = match database.prepare("SELECT command FROM history ORDER BY rowid") {
            Ok(statement) => statement,
            Err(error) => {
                eprintln!("history: failed to load persistent history: {error}");
                return;
            }
        };
        let rows = match statement.query_map([], |row| row.get::<_, String>(0)) {
            Ok(rows) => rows,
            Err(error) => {
                eprintln!("history: failed to load persistent history: {error}");
                return;
            }
        };
        for row in rows {
            match row {
                Ok(command) => self.history.borrow_mut().push(command),
                Err(error) => eprintln!("history: failed to load entry: {error}"),
            }
        }
    }

    fn load_timeline(&self, database: &Connection) {
        let mut statement = match database.prepare(
            "SELECT command, working_directory, duration_ms, status, policy, audit, effects, metadata
             FROM executions ORDER BY rowid",
        ) {
            Ok(statement) => statement,
            Err(error) => {
                eprintln!("timeline: failed to load persistent records: {error}");
                return;
            }
        };
        let rows = match statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i32>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        }) {
            Ok(rows) => rows,
            Err(error) => {
                eprintln!("timeline: failed to load persistent records: {error}");
                return;
            }
        };
        for row in rows {
            match row {
                Ok((
                    command,
                    working_directory,
                    duration_ms,
                    status,
                    policy,
                    audit,
                    effects,
                    metadata,
                )) => {
                    self.timeline.borrow_mut().push(record_from_storage(
                        command,
                        working_directory,
                        duration_ms,
                        status,
                        policy,
                        audit,
                        effects,
                        metadata,
                    ));
                }
                Err(error) => eprintln!("timeline: failed to load record: {error}"),
            }
        }
    }

    fn list_timeline(&self, args: &[String]) -> ExecutionResult {
        match args {
            [] => {}
            [command, term] if command == "search" => {
                return self.search_timeline(term);
            }
            [cmd, path @ ..] if cmd == "dir" || cmd == "--dir" => {
                let target_dir = if let Some(p) = path.first() {
                    PathBuf::from(p)
                } else {
                    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                };
                return self.filter_timeline_by_dir(&target_dir);
            }
            [cmd, status @ ..] if cmd == "status" || cmd == "exit" || cmd == "--status" || cmd == "--exit" || cmd == "--failed" => {
                let filter_failed = cmd == "--failed";
                let status_code = if filter_failed {
                    None
                } else if let Some(s) = status.first() {
                    s.parse::<i32>().ok()
                } else {
                    None
                };
                return self.filter_timeline_by_status(status_code, filter_failed);
            }
            _ => {
                eprintln!("timeline: usage: timeline [search <term> | dir [path] | status <code> | --failed]");
                return ExecutionResult::Failed;
            }
        }
        for (index, record) in self.timeline.borrow().iter().enumerate() {
            let redirections = if record.redirections.is_empty() {
                String::new()
            } else {
                format!("  redirects={:?}", record.redirections)
            };
            let stages = record
                .stages
                .iter()
                .map(|stage| format!("{}:{}", stage.program, stage.process_id.unwrap_or(0)))
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "{:>5}  {:>4} ms  [{}] {}  cwd={}  env={}  policy={} risk={} audit={:?} processes=[{}] effects={:?}{}",
                index + 1,
                record.duration_ms,
                record.status,
                record.command,
                record.working_directory.display(),
                record.environment_keys.len(),
                record.policy,
                record.risk,
                record.audit.outcome,
                stages,
                record.effects,
                redirections
            );
        }
        ExecutionResult::Builtin
    }

    fn foreground_job(&self, args: &[String]) -> ExecutionResult {
        let id = match parse_job_id(args, "fg") {
            Ok(id) => id,
            Err(error) => {
                eprintln!("{error}");
                return ExecutionResult::Failed;
            }
        };
        let position = self.jobs.borrow().iter().position(|job| job.id == id);
        let Some(position) = position else {
            eprintln!("fg: job {id} not found");
            return ExecutionResult::Failed;
        };
        let mut job = self.jobs.borrow_mut().remove(position);
        if let Err(error) = continue_job(&job.process_ids) {
            eprintln!("fg: {error}");
            self.jobs.borrow_mut().push(job);
            return ExecutionResult::Failed;
        }
        let process_id = job.process_ids[0];
        if job.process_ids.len() > 1 {
            let process_ids = job.process_ids.clone();
            let result =
                run_in_foreground_group(Some(process_id), || wait_for_pipeline_ids(&process_ids));
            return match result {
                Ok(PipelinePidWait::Completed(statuses)) => ExecutionResult::Pipeline(statuses),
                Ok(PipelinePidWait::Stopped) => {
                    job.state = JobState::Stopped;
                    self.jobs.borrow_mut().push(job);
                    ExecutionResult::Failed
                }
                Err(error) => {
                    eprintln!("fg: failed waiting for job {id}: {error}");
                    self.jobs.borrow_mut().push(job);
                    ExecutionResult::Failed
                }
            };
        }
        let result = run_in_foreground_group(Some(process_id), || wait_for_process(process_id));
        match result {
            Ok(WaitOutcome::Exited(status)) => ExecutionResult::External(status),
            Ok(WaitOutcome::Stopped) => {
                job.state = JobState::Stopped;
                self.jobs.borrow_mut().push(job);
                ExecutionResult::Failed
            }
            Err(error) => {
                eprintln!("fg: failed waiting for job {id}: {error}");
                self.jobs.borrow_mut().push(job);
                ExecutionResult::Failed
            }
        }
    }

    fn background_job(&self, args: &[String]) -> ExecutionResult {
        let id = match parse_job_id(args, "bg") {
            Ok(id) => id,
            Err(error) => {
                eprintln!("{error}");
                return ExecutionResult::Failed;
            }
        };
        let jobs = self.jobs.borrow();
        let Some(job) = jobs.iter().find(|job| job.id == id) else {
            eprintln!("bg: job {id} not found");
            return ExecutionResult::Failed;
        };
        let command = job.command.clone();
        if let Err(error) = continue_job(&job.process_ids) {
            eprintln!("bg: {error}");
            return ExecutionResult::Failed;
        }
        drop(jobs);
        if let Some(job) = self.jobs.borrow_mut().iter_mut().find(|job| job.id == id) {
            job.state = JobState::Running;
        }
        println!("[{id}] Running {command}");
        ExecutionResult::Builtin
    }
}

fn parse_job_id(args: &[String], command: &str) -> Result<usize, String> {
    let value = match args {
        [] => return Err(format!("{command}: job id required")),
        [value] => value.strip_prefix('%').unwrap_or(value),
        _ => return Err(format!("{command}: too many arguments")),
    };
    value
        .parse()
        .map_err(|_| format!("{command}: invalid job id: {value}"))
}

fn continue_job(process_ids: &[u32]) -> Result<(), String> {
    let process_group = -(process_ids[0] as libc::pid_t);
    let result = unsafe { libc::kill(process_group, libc::SIGCONT) };
    if result == -1 {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}

fn is_process_stopped(pid: u32) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some((_, fields)) = stat.split_once(") ") else {
        return false;
    };
    fields.split_whitespace().nth(1) == Some("T")
}

fn reset_child_signals(process: &mut Command, process_group: Option<u32>) {
    unsafe {
        process.pre_exec(move || {
            let group = process_group.map_or(0, |group| group as libc::pid_t);
            if libc::setpgid(0, group) == -1 {
                let _ = libc::setpgid(0, 0);
            }
            if libc::signal(libc::SIGINT, libc::SIG_DFL) == libc::SIG_ERR {
                return Err(std::io::Error::last_os_error());
            }
            if libc::signal(libc::SIGQUIT, libc::SIG_DFL) == libc::SIG_ERR {
                return Err(std::io::Error::last_os_error());
            }
            if libc::signal(libc::SIGTSTP, libc::SIG_DFL) == libc::SIG_ERR {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[derive(Debug)]
pub enum ExecutionResult {
    Builtin,
    BuiltinStatus(i32),
    External(ExitStatus),
    Pipeline(Vec<ExitStatus>),
    Background,
    Failed,
    CommandNotFound,
    Exit(i32),
}

impl ExecutionResult {
    pub fn status_code(&self) -> i32 {
        match self {
            Self::Builtin => 0,
            Self::BuiltinStatus(status) => *status,
            Self::External(status) => status_code(status),
            Self::Pipeline(statuses) => statuses.last().map(status_code).unwrap_or(1),
            Self::Background => 0,
            Self::Failed => 1,
            Self::CommandNotFound => 127,
            Self::Exit(status) => *status,
        }
    }
}

fn open_history_database() -> Result<Connection, String> {
    let path = if let Some(path) = env::var_os("SHELLPILOT_HISTORY_DB")
        .or_else(|| env::var_os("MSHELL_HISTORY_DB"))
        .or_else(|| env::var_os("MELCHIOR_HISTORY_DB"))
    {
        PathBuf::from(path)
    } else {
        let state = env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
            .ok_or_else(|| "HOME or XDG_STATE_HOME is not set".to_string())?;
        state.join("shellpilot").join("history.db")
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let database =
        Connection::open(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    database
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS history (
                 command TEXT NOT NULL,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS executions (
                 command TEXT NOT NULL,
                 working_directory TEXT NOT NULL,
                 duration_ms INTEGER NOT NULL,
                 status INTEGER NOT NULL,
                 policy TEXT NOT NULL,
                 audit TEXT NOT NULL,
                 effects TEXT NOT NULL,
                 metadata TEXT NOT NULL DEFAULT '{}',
                 created_at TEXT NOT NULL
             );",
        )
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let has_metadata = database
        .prepare("PRAGMA table_info(executions)")
        .and_then(|mut statement| {
            statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(|error| format!("{}: {error}", path.display()))?
        .iter()
        .any(|column| column == "metadata");
    if !has_metadata {
        database
            .execute(
                "ALTER TABLE executions ADD COLUMN metadata TEXT NOT NULL DEFAULT '{}'",
                [],
            )
            .map_err(|error| format!("{}: {error}", path.display()))?;
    }
    Ok(database)
}

fn timeline_metadata(record: &ExecutionRecord) -> Value {
    json!({
        "environment_keys": record.environment_keys,
        "redirections": record.redirections.iter().map(redirection_metadata).collect::<Vec<_>>(),
        "stages": record.stages.iter().map(|stage| json!({
            "program": stage.program,
            "process_id": stage.process_id,
        })).collect::<Vec<_>>(),
        "effects": record.effects.iter().map(effect_metadata).collect::<Vec<_>>(),
        "risk": record.risk.to_string(),
        "audit_decision": record.audit.decision.to_string(),
    })
}

fn redirection_metadata(redirection: &Redirection) -> Value {
    match redirection {
        Redirection::Stdin(path) => json!({"kind": "stdin", "path": path}),
        Redirection::Stdout(path) => json!({"kind": "stdout", "path": path}),
        Redirection::StdoutAppend(path) => json!({"kind": "stdout_append", "path": path}),
        Redirection::Stderr(path) => json!({"kind": "stderr", "path": path}),
        Redirection::StderrAppend(path) => json!({"kind": "stderr_append", "path": path}),
        Redirection::StdoutAndStderr(path) => json!({"kind": "stdout_and_stderr", "path": path}),
        Redirection::StdoutAndStderrAppend(path) => json!({"kind": "stdout_and_stderr_append", "path": path}),
        Redirection::DupRead(src, dst) => json!({"kind": "dup_read", "src": src, "dst": dst}),
        Redirection::DupWrite(src, dst) => json!({"kind": "dup_write", "src": src, "dst": dst}),
        Redirection::HereString(content) => json!({"kind": "here_string", "content": content}),
    }
}

fn effect_metadata(effect: &Effect) -> Value {
    match effect {
        Effect::NetworkAccess => json!({"kind": "network_access"}),
        Effect::PrivilegeChange => json!({"kind": "privilege_change"}),
        Effect::ProcessCreation => json!({"kind": "process_creation"}),
        Effect::PipelineDataFlow => json!({"kind": "pipeline_data_flow"}),
        Effect::FilesystemRead(path) => json!({"kind": "filesystem_read", "path": path}),
        Effect::FilesystemWrite(path) => json!({"kind": "filesystem_write", "path": path}),
        Effect::FilesystemDelete(path) => json!({"kind": "filesystem_delete", "path": path}),
        Effect::SensitivePathRead(path) => json!({"kind": "sensitive_path_read", "path": path}),
        Effect::SensitivePathWrite(path) => json!({"kind": "sensitive_path_write", "path": path}),
        Effect::UntrustedPipelineExecution(cmd) => json!({"kind": "untrusted_pipeline_execution", "cmd": cmd}),
        Effect::PackageInstallation(pkg) => json!({"kind": "package_installation", "pkg": pkg}),
        Effect::ProcessKill(target) => json!({"kind": "process_kill", "target": target}),
    }
}

#[allow(clippy::too_many_arguments)]
fn record_from_storage(
    command: String,
    working_directory: String,
    duration_ms: i64,
    status: i32,
    policy: String,
    audit: String,
    effects: String,
    metadata: String,
) -> ExecutionRecord {
    let metadata = serde_json::from_str::<Value>(&metadata).unwrap_or_default();
    let stored_effects = metadata
        .get("effects")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(effect_from_metadata).collect())
        .unwrap_or_else(|| parse_effects_debug(&effects));
    let decision = parse_policy_decision(
        metadata
            .get("audit_decision")
            .and_then(Value::as_str)
            .unwrap_or(&policy),
    );
    let audit_outcome = parse_audit_outcome(&audit);
    let stages = metadata
        .get("stages")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| {
                    Some(ProcessRecord {
                        program: value.get("program")?.as_str()?.to_owned(),
                        process_id: value
                            .get("process_id")
                            .and_then(Value::as_u64)
                            .map(|id| id as u32),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let redirections = metadata
        .get("redirections")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(redirection_from_metadata)
                .collect()
        })
        .unwrap_or_default();
    let risk = match metadata.get("risk").and_then(Value::as_str) {
        Some("MEDIUM") => RiskLevel::Medium,
        Some("HIGH") => RiskLevel::High,
        _ => policy::risk_level(&stored_effects),
    };
    let environment_keys = metadata
        .get("environment_keys")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    ExecutionRecord {
        command,
        working_directory: PathBuf::from(working_directory),
        duration_ms: duration_ms.max(0) as u128,
        status,
        environment_keys,
        redirections,
        stages,
        effects: stored_effects,
        policy: decision,
        risk,
        audit: AuditEvent {
            decision,
            outcome: audit_outcome,
        },
    }
}

fn redirection_from_metadata(value: &Value) -> Option<Redirection> {
    let kind = value.get("kind")?.as_str()?;
    match kind {
        "stdin" => Some(Redirection::Stdin(value.get("path")?.as_str()?.to_owned())),
        "stdout" => Some(Redirection::Stdout(value.get("path")?.as_str()?.to_owned())),
        "stdout_append" => Some(Redirection::StdoutAppend(value.get("path")?.as_str()?.to_owned())),
        "stderr" => Some(Redirection::Stderr(value.get("path")?.as_str()?.to_owned())),
        "stderr_append" => Some(Redirection::StderrAppend(value.get("path")?.as_str()?.to_owned())),
        "stdout_and_stderr" => Some(Redirection::StdoutAndStderr(value.get("path")?.as_str()?.to_owned())),
        "stdout_and_stderr_append" => Some(Redirection::StdoutAndStderrAppend(value.get("path")?.as_str()?.to_owned())),
        "dup_read" => Some(Redirection::DupRead(
            value.get("src")?.as_i64()? as i32,
            value.get("dst")?.as_i64()? as i32,
        )),
        "dup_write" => Some(Redirection::DupWrite(
            value.get("src")?.as_i64()? as i32,
            value.get("dst")?.as_i64()? as i32,
        )),
        "here_string" => Some(Redirection::HereString(value.get("content")?.as_str()?.to_owned())),
        _ => None,
    }
}

fn effect_from_metadata(value: &Value) -> Option<Effect> {
    let kind = value.get("kind")?.as_str()?;
    let path = || value.get("path")?.as_str().map(str::to_owned);
    match kind {
        "network_access" => Some(Effect::NetworkAccess),
        "privilege_change" => Some(Effect::PrivilegeChange),
        "process_creation" => Some(Effect::ProcessCreation),
        "pipeline_data_flow" => Some(Effect::PipelineDataFlow),
        "filesystem_read" => Some(Effect::FilesystemRead(path()?)),
        "filesystem_write" => Some(Effect::FilesystemWrite(path()?)),
        "filesystem_delete" => Some(Effect::FilesystemDelete(path()?)),
        "sensitive_path_read" => Some(Effect::SensitivePathRead(path()?)),
        "sensitive_path_write" => Some(Effect::SensitivePathWrite(path()?)),
        "untrusted_pipeline_execution" => {
            Some(Effect::UntrustedPipelineExecution(value.get("cmd")?.as_str()?.to_owned()))
        }
        "package_installation" => {
            Some(Effect::PackageInstallation(value.get("pkg")?.as_str()?.to_owned()))
        }
        "process_kill" => {
            Some(Effect::ProcessKill(value.get("target")?.as_str()?.to_owned()))
        }
        _ => None,
    }
}

fn parse_policy_decision(value: &str) -> PolicyDecision {
    match value {
        "ASK" => PolicyDecision::Ask,
        "BLOCK" => PolicyDecision::Block,
        _ => PolicyDecision::Allow,
    }
}

fn parse_audit_outcome(value: &str) -> AuditOutcome {
    match value {
        "Blocked" => AuditOutcome::Blocked,
        "Denied" => AuditOutcome::Denied,
        _ => AuditOutcome::Executed,
    }
}

fn parse_effects_debug(value: &str) -> Vec<Effect> {
    value
        .trim_matches(['[', ']'])
        .split(", ")
        .filter_map(|entry| {
            if entry == "NetworkAccess" {
                return Some(Effect::NetworkAccess);
            }
            if entry == "PrivilegeChange" {
                return Some(Effect::PrivilegeChange);
            }
            if entry == "ProcessCreation" {
                return Some(Effect::ProcessCreation);
            }
            if entry == "PipelineDataFlow" {
                return Some(Effect::PipelineDataFlow);
            }
            let (kind, path) = entry.split_once('(')?;
            let path = path.strip_prefix('"')?.strip_suffix("\")")?.to_owned();
            match kind {
                "FilesystemRead" => Some(Effect::FilesystemRead(path)),
                "FilesystemWrite" => Some(Effect::FilesystemWrite(path)),
                "FilesystemDelete" => Some(Effect::FilesystemDelete(path)),
                "SensitivePathRead" => Some(Effect::SensitivePathRead(path)),
                "SensitivePathWrite" => Some(Effect::SensitivePathWrite(path)),
                "UntrustedPipelineExecution" => Some(Effect::UntrustedPipelineExecution(path)),
                "PackageInstallation" => Some(Effect::PackageInstallation(path)),
                "ProcessKill" => Some(Effect::ProcessKill(path)),
                _ => None,
            }
        })
        .collect()
}

fn list_processes(args: &[String]) -> ExecutionResult {
    if !args.is_empty() {
        eprintln!("processes: too many arguments");
        return ExecutionResult::Failed;
    }

    let entries = match fs::read_dir("/proc") {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("processes: cannot inspect /proc: {error}");
            return ExecutionResult::Failed;
        }
    };
    let mut processes = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                eprintln!("processes: cannot read /proc entry: {error}");
                continue;
            }
        };
        let pid = match entry.file_name().to_string_lossy().parse::<u32>() {
            Ok(pid) => pid,
            Err(_) => continue,
        };
        let command_path = entry.path().join("comm");
        let command = match fs::read_to_string(&command_path) {
            Ok(command) => command.trim().to_owned(),
            Err(_) => continue,
        };
        processes.push((pid, command));
    }
    processes.sort_unstable_by_key(|(pid, _)| *pid);
    println!("{:>6}  COMMAND", "PID");
    for (pid, command) in processes {
        println!("{pid:>6}  {command}");
    }
    ExecutionResult::Builtin
}

fn list_children(args: &[String]) -> ExecutionResult {
    let parent_pid = match args {
        [] => std::process::id(),
        [pid] => match pid.parse::<u32>() {
            Ok(pid) => pid,
            Err(_) => {
                eprintln!("children: expected a numeric PID");
                return ExecutionResult::Failed;
            }
        },
        _ => {
            eprintln!("children: expected zero or one PID");
            return ExecutionResult::Failed;
        }
    };

    let entries = match fs::read_dir("/proc") {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("children: cannot inspect /proc: {error}");
            return ExecutionResult::Failed;
        }
    };
    let mut children = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                eprintln!("children: cannot read /proc entry: {error}");
                continue;
            }
        };
        let pid = match entry.file_name().to_string_lossy().parse::<u32>() {
            Ok(pid) => pid,
            Err(_) => continue,
        };
        let stat = match fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(_) => continue,
        };
        let Some((_, fields)) = stat.split_once(") ") else {
            continue;
        };
        let fields = fields.split_whitespace().collect::<Vec<_>>();
        if fields.get(1).and_then(|value| value.parse::<u32>().ok()) != Some(parent_pid) {
            continue;
        }
        let command = stat
            .split_once('(')
            .and_then(|(_, rest)| rest.rsplit_once(") "))
            .map(|(command, _)| command)
            .unwrap_or("?");
        children.push((pid, command.to_owned()));
    }
    children.sort_unstable_by_key(|(pid, _)| *pid);
    println!("{:>6}  COMMAND", "PID");
    for (pid, command) in children {
        println!("{pid:>6}  {command}");
    }
    ExecutionResult::Builtin
}

fn format_json(args: &[String]) -> ExecutionResult {
    if args.is_empty() {
        eprintln!("json: value required");
        return ExecutionResult::Failed;
    }
    let input = args.join(" ");
    let value = match serde_json::from_str::<serde_json::Value>(&input) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("json: invalid value: {error}");
            return ExecutionResult::Failed;
        }
    };
    match serde_json::to_string_pretty(&value) {
        Ok(output) => {
            println!("{output}");
            ExecutionResult::Builtin
        }
        Err(error) => {
            eprintln!("json: formatting failed: {error}");
            ExecutionResult::Failed
        }
    }
}

fn format_yaml(args: &[String]) -> ExecutionResult {
    if args.is_empty() {
        eprintln!("yaml: value required");
        return ExecutionResult::Failed;
    }
    let input = args.join(" ");
    let value = match serde_yaml::from_str::<serde_yaml::Value>(&input) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("yaml: invalid value: {error}");
            return ExecutionResult::Failed;
        }
    };
    match serde_yaml::to_string(&value) {
        Ok(output) => {
            print!("{output}");
            ExecutionResult::Builtin
        }
        Err(error) => {
            eprintln!("yaml: formatting failed: {error}");
            ExecutionResult::Failed
        }
    }
}

fn format_toml(args: &[String]) -> ExecutionResult {
    if args.is_empty() {
        eprintln!("toml: value required");
        return ExecutionResult::Failed;
    }
    let input = args.join(" ");
    let value = match toml::from_str::<toml::Table>(&input) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("toml: invalid value: {error}");
            return ExecutionResult::Failed;
        }
    };
    match toml::to_string_pretty(&value) {
        Ok(output) => {
            print!("{output}");
            ExecutionResult::Builtin
        }
        Err(error) => {
            eprintln!("toml: formatting failed: {error}");
            ExecutionResult::Failed
        }
    }
}

fn show_help(args: &[String]) -> ExecutionResult {
    if !args.is_empty() {
        eprintln!("help: too many arguments");
        return ExecutionResult::Failed;
    }
    println!("shellpilot builtins:");
    println!("  :                         no-op");
    println!("  cd [DIR|-]                change directory");
    println!("  pwd                       print directory");
    println!("  echo [-n] [ARGS...]       print arguments");
    println!("  printf FORMAT [ARGS...]  format output");
    println!("  true | false              return a success or failure status");
    println!("  export [NAME=VALUE...]    set shell environment variables");
    println!("  unset NAME...             remove shell environment variables");
    println!("  env [NAME=VALUE ...] [COMMAND]  inspect or scope environment");
    println!("  exit [STATUS]             exit the shell");
    println!("  alias [NAME[=VALUE]...]   view or define command aliases");
    println!("  unalias [-a] NAME...      remove command aliases");
    println!("  type COMMAND...            inspect commands and report lookup failures");
    println!("  command [-v|-V] NAME...    inspect command resolution");
    println!("  jobs | wait [%ID] | kill %ID | fg %ID | bg %ID  control jobs");
    println!("  history [N|clear|search|dir|status] view or filter history");
    println!("  timeline [search|dir|status] view or filter execution records");
    println!("  explain COMMAND...        analyze command effects");
    println!("  policy COMMAND...         evaluate command policy");
    println!("  processes                 list Linux processes");
    println!("  network | ports           inspect TCP sockets");
    println!("  connections               list established TCP connections");
    println!("  children [PID]            list child processes");
    println!("  json | yaml | toml VALUE  validate and format data");
    println!("  tutor [start|check|hint|solution|reset|next] interactive academy");
    println!("  whatif COMMAND...         dry-run preview of command effects");
    println!("  undo [diff]               revert workspace or preview changes");
    println!("  snapshot [NAME]           save named sandbox checkpoint");
    println!("  tree [PATH] [-L N] [-d]   visual directory tree");
    println!("  cheat [TOOL]              offline Unix tool cheatsheets");
    println!("  doctor                    diagnose last failed command");
    println!("  service [start|stop...]   manage simulated server daemons");
    println!("  curl [-I|-s] URL          query mock local services");
    println!("  cadet | profile           view flight dossier, XP, and badges");
    println!("  drill [start|check|hint]  emergency incident drills");
    ExecutionResult::Builtin
}

fn list_network(args: &[String]) -> ExecutionResult {
    if !args.is_empty() {
        eprintln!("network: too many arguments");
        return ExecutionResult::Failed;
    }

    let mut sockets = Vec::new();
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) => {
                eprintln!("network: cannot read {path}: {error}");
                return ExecutionResult::Failed;
            }
        };
        for line in contents.lines().skip(1) {
            match parse_proc_socket(line, path.ends_with("tcp6")) {
                Ok(socket) => sockets.push(socket),
                Err(error) => eprintln!("network: skipping malformed entry: {error}"),
            }
        }
    }

    sockets.sort();
    println!("{:<8}  {:<22}  {:<22}", "STATE", "LOCAL", "REMOTE");
    for (state, local, remote) in sockets {
        println!("{state:<8}  {local:<22}  {remote:<22}");
    }
    ExecutionResult::Builtin
}

fn list_ports(args: &[String]) -> ExecutionResult {
    if !args.is_empty() {
        eprintln!("ports: too many arguments");
        return ExecutionResult::Failed;
    }

    let mut ports = Vec::new();
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) => {
                eprintln!("ports: cannot read {path}: {error}");
                return ExecutionResult::Failed;
            }
        };
        for line in contents.lines().skip(1) {
            let socket = match parse_proc_socket(line, path.ends_with("tcp6")) {
                Ok(socket) => socket,
                Err(error) => {
                    eprintln!("ports: skipping malformed entry: {error}");
                    continue;
                }
            };
            if socket.0 == "LISTEN" {
                ports.push(socket.1);
            }
        }
    }

    ports.sort();
    ports.dedup();
    println!("LISTENING");
    for port in ports {
        println!("{port}");
    }
    ExecutionResult::Builtin
}

fn list_connections(args: &[String]) -> ExecutionResult {
    if !args.is_empty() {
        eprintln!("connections: too many arguments");
        return ExecutionResult::Failed;
    }

    let mut connections = Vec::new();
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) => {
                eprintln!("connections: cannot read {path}: {error}");
                return ExecutionResult::Failed;
            }
        };
        for line in contents.lines().skip(1) {
            match parse_proc_socket(line, path.ends_with("tcp6")) {
                Ok((state, local, remote)) if state == "ESTABLISHED" => {
                    connections.push((local, remote));
                }
                Ok(_) => {}
                Err(error) => eprintln!("connections: skipping malformed entry: {error}"),
            }
        }
    }

    connections.sort();
    println!("{:<22}  {:<22}", "LOCAL", "REMOTE");
    for (local, remote) in connections {
        println!("{local:<22}  {remote:<22}");
    }
    ExecutionResult::Builtin
}

fn parse_proc_socket(line: &str, ipv6: bool) -> Result<(String, String, String), String> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    let local = fields.get(1).ok_or("missing local endpoint")?;
    let remote = fields.get(2).ok_or("missing remote endpoint")?;
    let state = fields.get(3).ok_or("missing socket state")?;
    let state = match *state {
        "01" => "ESTABLISHED",
        "0A" => "LISTEN",
        "02" => "SYN_SENT",
        "03" => "SYN_RECV",
        "04" => "FIN_WAIT1",
        "05" => "FIN_WAIT2",
        "06" => "TIME_WAIT",
        "07" => "CLOSE",
        "08" => "CLOSE_WAIT",
        "09" => "LAST_ACK",
        "0B" => "CLOSING",
        other => other,
    };
    Ok((
        state.to_owned(),
        decode_proc_endpoint(local, ipv6)?,
        decode_proc_endpoint(remote, ipv6)?,
    ))
}

fn decode_proc_endpoint(value: &str, ipv6: bool) -> Result<String, String> {
    let (address, port) = value.rsplit_once(':').ok_or("missing endpoint port")?;
    let port = u16::from_str_radix(port, 16).map_err(|_| "invalid endpoint port")?;
    if ipv6 {
        let bytes = address.as_bytes();
        if bytes.len() != 32 {
            return Err("invalid IPv6 endpoint address".into());
        }
        let mut octets = [0u8; 16];
        for (index, chunk) in bytes.as_chunks::<2>().0.iter().enumerate() {
            octets[index] = u8::from_str_radix(
                std::str::from_utf8(chunk).map_err(|_| "invalid IPv6 endpoint address")?,
                16,
            )
            .map_err(|_| "invalid IPv6 endpoint address")?;
        }
        for chunk in octets.as_chunks_mut::<4>().0 {
            chunk.reverse();
        }
        Ok(format!("[{}]:{port}", std::net::Ipv6Addr::from(octets)))
    } else {
        if address.len() != 8 {
            return Err("invalid IPv4 endpoint address".into());
        }
        let address = u32::from_str_radix(address, 16)
            .map_err(|_| "invalid IPv4 endpoint address")?
            .to_le_bytes();
        Ok(format!(
            "{}.{}.{}.{}:{port}",
            address[0], address[1], address[2], address[3]
        ))
    }
}

fn format_command(command: &ParsedCommand) -> String {
    command
        .environment
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .chain(std::iter::once(command.program.clone()))
        .chain(command.args.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn effective_environment_keys(pipeline: &ParsedPipeline) -> Vec<String> {
    let mut keys = env::vars_os()
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>();
    for command in &pipeline.commands {
        keys.extend(command.environment.iter().map(|(name, _)| name.clone()));
    }
    keys.into_iter().collect()
}

fn is_valid_environment_name(name: &str) -> bool {
    let mut characters = name.chars();
    matches!(characters.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn status_code(status: &ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(1)
}

enum SpawnError {
    CommandNotFound,
    Failed,
}

fn spawn_pipeline_processes(pipeline: &ParsedPipeline) -> Result<Vec<ProcessHandle>, SpawnError> {
    let mut processes = Vec::with_capacity(pipeline.commands.len());
    let mut previous_stdout: Option<RawFd> = None;
    let mut process_group: Option<u32> = None;
    let num_cmds = pipeline.commands.len();

    for (index, command) in pipeline.commands.iter().enumerate() {
        let is_last = index + 1 == num_cmds;

        let mut current_pipe = [-1, -1];
        if !is_last
            && unsafe { libc::pipe2(current_pipe.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
                terminate_processes(&mut processes);
                return Err(SpawnError::Failed);
            }

        let in_fd = previous_stdout;
        let out_fd = if !is_last { Some(current_pipe[1]) } else { None };

        if builtins::is_builtin(&command.program) {
            let pid = unsafe { libc::fork() };
            if pid < 0 {
                terminate_processes(&mut processes);
                return Err(SpawnError::Failed);
            }
            if pid == 0 {
                let group = process_group.map_or(0, |g| g as libc::pid_t);
                unsafe {
                    libc::setpgid(0, group);
                    libc::signal(libc::SIGINT, libc::SIG_DFL);
                    libc::signal(libc::SIGQUIT, libc::SIG_DFL);
                    libc::signal(libc::SIGTSTP, libc::SIG_DFL);
                    libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                }
                if let Some(fd_in) = in_fd {
                    unsafe {
                        libc::dup2(fd_in, libc::STDIN_FILENO);
                        libc::close(fd_in);
                    }
                }
                if let Some(fd_out) = out_fd {
                    unsafe {
                        libc::dup2(fd_out, libc::STDOUT_FILENO);
                        libc::close(fd_out);
                    }
                }
                if !is_last {
                    unsafe { libc::close(current_pipe[0]); }
                }
                for (name, val) in &command.environment {
                    unsafe { env::set_var(name, val); }
                }
                let _redirs = match BuiltinRedirections::apply(&command.redirects) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("{}: {e}", command.program);
                        unsafe { libc::_exit(1); }
                    }
                };
                let res = builtins::execute(&command.program, &command.args, find_executable_in_path);
                use std::io::Write;
                let _ = std::io::stdout().flush();
                let _ = std::io::stderr().flush();
                let code = match res {
                    Ok(builtins::BuiltinResult::Handled) => 0,
                    Ok(builtins::BuiltinResult::Status(s)) => s,
                    Ok(builtins::BuiltinResult::Exit(s)) => s,
                    Err(err) => {
                        eprintln!("{err}");
                        1
                    }
                };
                unsafe { libc::_exit(code); }
            }

            let child_pid = pid as u32;
            process_group.get_or_insert(child_pid);
            if let Some(fd_in) = in_fd {
                unsafe { libc::close(fd_in); }
            }
            if let Some(fd_out) = out_fd {
                unsafe { libc::close(fd_out); }
            }
            if !is_last {
                previous_stdout = Some(current_pipe[0]);
            }
            processes.push(ProcessHandle::Forked(child_pid));
        } else {
            let executable = match find_executable_in_path(&command.program) {
                Some(path) => path,
                None => {
                    eprintln!("{}: command not found", command.program);
                    terminate_processes(&mut processes);
                    return Err(SpawnError::CommandNotFound);
                }
            };
            let mut proc = Command::new(executable);
            proc.args(&command.args);
            proc.envs(command.environment.iter().map(|(k, v)| (k, v)));
            reset_child_signals(&mut proc, process_group);

            if let Some(fd_in) = in_fd {
                proc.stdin(unsafe { Stdio::from(File::from_raw_fd(fd_in)) });
            }
            if let Some(fd_out) = out_fd {
                proc.stdout(unsafe { Stdio::from(File::from_raw_fd(fd_out)) });
            }
            if let Err(err) = apply_redirections(&mut proc, &command.redirects) {
                eprintln!("{}: {err}", command.program);
                terminate_processes(&mut processes);
                return Err(SpawnError::Failed);
            }

            let child = match proc.spawn() {
                Ok(child) => child,
                Err(error) => {
                    eprintln!("{}: failed to execute: {error}", command.program);
                    terminate_processes(&mut processes);
                    return Err(SpawnError::Failed);
                }
            };

            process_group.get_or_insert(child.id());
            if !is_last {
                previous_stdout = Some(current_pipe[0]);
            }
            processes.push(ProcessHandle::External(child));
        }
    }

    Ok(processes)
}

fn terminate_processes(processes: &mut [ProcessHandle]) {
    for process in processes {
        let _ = process.kill();
        let _ = process.wait();
    }
}

fn apply_redirections(process: &mut Command, redirects: &[Redirection]) -> Result<(), String> {
    for redirect in redirects {
        match redirect {
            Redirection::Stdin(path) => {
                process.stdin(Stdio::from(open_input(path)?));
            }
            Redirection::Stdout(path) => {
                process.stdout(Stdio::from(open_output(path, false)?));
            }
            Redirection::StdoutAppend(path) => {
                process.stdout(Stdio::from(open_output(path, true)?));
            }
            Redirection::Stderr(path) => {
                process.stderr(Stdio::from(open_output(path, false)?));
            }
            Redirection::StderrAppend(path) => {
                process.stderr(Stdio::from(open_output(path, true)?));
            }
            Redirection::StdoutAndStderr(path) => {
                let file1 = open_output(path, false)?;
                let file2 = file1.try_clone().map_err(|e| e.to_string())?;
                process.stdout(Stdio::from(file1));
                process.stderr(Stdio::from(file2));
            }
            Redirection::StdoutAndStderrAppend(path) => {
                let file1 = open_output(path, true)?;
                let file2 = file1.try_clone().map_err(|e| e.to_string())?;
                process.stdout(Stdio::from(file1));
                process.stderr(Stdio::from(file2));
            }
            Redirection::HereString(content) => {
                let mut fds = [0; 2];
                if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                let mut write_file = unsafe { File::from_raw_fd(fds[1]) };
                use std::io::Write;
                let _ = write_file.write_all(content.as_bytes());
                let _ = write_file.write_all(b"\n");
                drop(write_file);
                process.stdin(unsafe { Stdio::from(File::from_raw_fd(fds[0])) });
            }
            Redirection::DupWrite(src, dst) => {
                let src_fd = *src;
                let dst_fd = *dst;
                unsafe {
                    process.pre_exec(move || {
                        if libc::dup2(dst_fd, src_fd) == -1 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
            }
            Redirection::DupRead(src, dst) => {
                let src_fd = *src;
                let dst_fd = *dst;
                unsafe {
                    process.pre_exec(move || {
                        if libc::dup2(dst_fd, src_fd) == -1 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
            }
        }
    }
    Ok(())
}

struct BuiltinRedirections {
    saved: Vec<(RawFd, RawFd)>,
}

type RawFd = std::os::unix::io::RawFd;

struct ScopedEnvironment {
    previous: Vec<(String, Option<std::ffi::OsString>)>,
}

impl ScopedEnvironment {
    fn apply(assignments: &[(String, String)]) -> Result<Self, String> {
        let mut previous = Vec::with_capacity(assignments.len());
        for (name, value) in assignments {
            previous.push((name.clone(), env::var_os(name)));
            unsafe { env::set_var(name, value) };
        }
        Ok(Self { previous })
    }
}

impl Drop for ScopedEnvironment {
    fn drop(&mut self) {
        for (name, value) in self.previous.drain(..).rev() {
            unsafe {
                match value {
                    Some(value) => env::set_var(name, value),
                    None => env::remove_var(name),
                }
            }
        }
    }
}

impl BuiltinRedirections {
    fn apply(redirects: &[Redirection]) -> Result<Self, String> {
        let mut saved = Vec::new();
        for redirect in redirects {
            match redirect {
                Redirection::Stdin(path) => {
                    let file = File::open(path).map_err(|e| format!("{path}: {e}"))?;
                    Self::dup_save(&mut saved, libc::STDIN_FILENO, file.as_raw_fd())?;
                }
                Redirection::Stdout(path) => {
                    let file = open_output(path, false)?;
                    Self::dup_save(&mut saved, libc::STDOUT_FILENO, file.as_raw_fd())?;
                }
                Redirection::StdoutAppend(path) => {
                    let file = open_output(path, true)?;
                    Self::dup_save(&mut saved, libc::STDOUT_FILENO, file.as_raw_fd())?;
                }
                Redirection::Stderr(path) => {
                    let file = open_output(path, false)?;
                    Self::dup_save(&mut saved, libc::STDERR_FILENO, file.as_raw_fd())?;
                }
                Redirection::StderrAppend(path) => {
                    let file = open_output(path, true)?;
                    Self::dup_save(&mut saved, libc::STDERR_FILENO, file.as_raw_fd())?;
                }
                Redirection::StdoutAndStderr(path) => {
                    let file = open_output(path, false)?;
                    Self::dup_save(&mut saved, libc::STDOUT_FILENO, file.as_raw_fd())?;
                    Self::dup_save(&mut saved, libc::STDERR_FILENO, file.as_raw_fd())?;
                }
                Redirection::StdoutAndStderrAppend(path) => {
                    let file = open_output(path, true)?;
                    Self::dup_save(&mut saved, libc::STDOUT_FILENO, file.as_raw_fd())?;
                    Self::dup_save(&mut saved, libc::STDERR_FILENO, file.as_raw_fd())?;
                }
                Redirection::HereString(content) => {
                    let mut fds = [0; 2];
                    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
                        return Err(std::io::Error::last_os_error().to_string());
                    }
                    let mut write_file = unsafe { File::from_raw_fd(fds[1]) };
                    use std::io::Write;
                    let _ = write_file.write_all(content.as_bytes());
                    let _ = write_file.write_all(b"\n");
                    drop(write_file);
                    Self::dup_save(&mut saved, libc::STDIN_FILENO, fds[0])?;
                    unsafe { libc::close(fds[0]); }
                }
                Redirection::DupWrite(src, dst) | Redirection::DupRead(src, dst) => {
                    Self::dup_save(&mut saved, *src, *dst)?;
                }
            }
        }
        Ok(Self { saved })
    }

    fn dup_save(saved: &mut Vec<(RawFd, RawFd)>, target: RawFd, new_fd: RawFd) -> Result<(), String> {
        let original = unsafe { libc::dup(target) };
        if original == -1 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if unsafe { libc::dup2(new_fd, target) } == -1 {
            unsafe { libc::close(original); }
            return Err(std::io::Error::last_os_error().to_string());
        }
        saved.push((target, original));
        Ok(())
    }
}

impl Drop for BuiltinRedirections {
    fn drop(&mut self) {
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let _ = std::io::Write::flush(&mut std::io::stderr());
        for (target, original) in self.saved.drain(..).rev() {
            unsafe {
                libc::dup2(original, target);
                libc::close(original);
            }
        }
    }
}

fn open_input(path: &str) -> Result<File, String> {
    File::open(path).map_err(|error| format!("{path}: {error}"))
}

fn open_output(path: &str, append: bool) -> Result<File, String> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(!append)
        .append(append)
        .open(path)
        .map_err(|error| format!("{path}: {error}"))
}

enum WaitOutcome {
    Exited(ExitStatus),
    Stopped,
}

enum PipelineWaitOutcome {
    Completed(Vec<ExitStatus>),
    Stopped {
        children: Vec<ProcessHandle>,
        statuses: Vec<ExitStatus>,
    },
}

fn wait_for_pipeline(children: Vec<ProcessHandle>) -> Result<PipelineWaitOutcome, std::io::Error> {
    let process_ids = children.iter().map(ProcessHandle::id).collect::<Vec<_>>();
    let mut statuses = Vec::with_capacity(process_ids.len());
    for process_id in process_ids {
        match wait_for_process(process_id)? {
            WaitOutcome::Exited(status) => statuses.push(status),
            WaitOutcome::Stopped => {
                return Ok(PipelineWaitOutcome::Stopped { children, statuses });
            }
        }
    }
    Ok(PipelineWaitOutcome::Completed(statuses))
}

pub fn execute_capture(command_str: &str, last_status: i32) -> Result<String, String> {
    let ast = match crate::parser::parse_ast(command_str, last_status)? {
        Some(ast) => ast,
        None => return Ok(String::new()),
    };

    let mut fds = [0; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
        return Err(std::io::Error::last_os_error().to_string());
    }

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        return Err(std::io::Error::last_os_error().to_string());
    }

    if pid == 0 {
        unsafe {
            libc::close(fds[0]);
            libc::dup2(fds[1], libc::STDOUT_FILENO);
            libc::close(fds[1]);
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGQUIT, libc::SIG_DFL);
            libc::signal(libc::SIGTSTP, libc::SIG_DFL);
        }
        let executor = Executor::new();
        let result = executor.execute_ast(&ast);
        unsafe { libc::_exit(result.status_code()); }
    }

    unsafe { libc::close(fds[1]); }
    let mut file = unsafe { File::from_raw_fd(fds[0]) };
    let mut output = String::new();
    use std::io::Read;
    let _ = file.read_to_string(&mut output);
    let mut status = 0;
    unsafe { libc::waitpid(pid, &mut status, 0); }

    while output.ends_with('\n') || output.ends_with('\r') {
        output.pop();
    }
    Ok(output)
}

fn parse_structured_data(raw: &str) -> Result<serde_json::Value, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(serde_json::Value::Null);
    }

    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return Ok(v);
    }

    let non_empty_lines: Vec<&str> = trimmed
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();

    if non_empty_lines.len() > 1 {
        let mut jsonl_items = Vec::new();
        let mut all_json = true;
        for line in &non_empty_lines {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                jsonl_items.push(v);
            } else {
                all_json = false;
                break;
            }
        }
        if all_json && !jsonl_items.is_empty() {
            return Ok(serde_json::Value::Array(jsonl_items));
        }
    }

    if let Ok(v) = serde_yaml::from_str::<serde_json::Value>(trimmed)
        && matches!(v, serde_json::Value::Object(_) | serde_json::Value::Array(_)) {
            return Ok(v);
        }

    let lines: Vec<serde_json::Value> = non_empty_lines
        .into_iter()
        .map(|l| serde_json::Value::String(l.to_string()))
        .collect();
    Ok(serde_json::Value::Array(lines))
}

fn project_structured_value(val: &serde_json::Value, path: &str) -> serde_json::Value {
    let clean_path = path.trim_start_matches('.');
    if clean_path.is_empty() {
        return val.clone();
    }

    let parts: Vec<&str> = clean_path.split('.').collect();
    let mut current = val.clone();
    for part in parts {
        current = project_single_part(&current, part);
    }
    current
}

fn project_single_part(val: &serde_json::Value, part: &str) -> serde_json::Value {
    if let Some(bracket_idx) = part.find('[') {
        let field = &part[..bracket_idx];
        let rest = &part[bracket_idx..];
        let obj_part = if field.is_empty() {
            val.clone()
        } else {
            lookup_field(val, field)
        };
        if let Some(end_bracket) = rest.find(']')
            && let Ok(idx) = rest[1..end_bracket].parse::<usize>()
                && let serde_json::Value::Array(arr) = obj_part {
                    return arr.get(idx).cloned().unwrap_or(serde_json::Value::Null);
                }
        serde_json::Value::Null
    } else {
        match val {
            serde_json::Value::Array(arr) => {
                let projected: Vec<serde_json::Value> = arr
                    .iter()
                    .map(|elem| lookup_field(elem, part))
                    .filter(|v| !v.is_null())
                    .collect();
                serde_json::Value::Array(projected)
            }
            _ => lookup_field(val, part),
        }
    }
}

fn lookup_field(val: &serde_json::Value, key: &str) -> serde_json::Value {
    match val {
        serde_json::Value::Object(map) => {
            if let Some(v) = map.get(key) {
                return v.clone();
            }
            if let Some((_, v)) = map.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
                return v.clone();
            }
            serde_json::Value::Null
        }
        _ => serde_json::Value::Null,
    }
}

fn filter_structured_value(
    val: &serde_json::Value,
    expr: &crate::parser::FilterExpr,
) -> serde_json::Value {
    let field = expr.field.trim_start_matches('.');

    match val {
        serde_json::Value::Array(arr) => {
            let filtered: Vec<serde_json::Value> = arr
                .iter()
                .filter(|elem| evaluate_filter_on_elem(elem, field, expr))
                .cloned()
                .collect();
            serde_json::Value::Array(filtered)
        }
        serde_json::Value::Object(_) => {
            if evaluate_filter_on_elem(val, field, expr) {
                val.clone()
            } else {
                serde_json::Value::Null
            }
        }
        _ => val.clone(),
    }
}

fn evaluate_filter_on_elem(
    elem: &serde_json::Value,
    field: &str,
    expr: &crate::parser::FilterExpr,
) -> bool {
    let actual_val = lookup_field(elem, field);
    if actual_val.is_null() {
        return matches!(expr.op, crate::parser::FilterOp::NotEqual);
    }

    match expr.op {
        crate::parser::FilterOp::Exists => !actual_val.is_null(),
        crate::parser::FilterOp::Equal => {
            if let Some(s) = actual_val.as_str() {
                s == expr.value
            } else if let Some(n) = actual_val.as_f64() {
                expr.value
                    .parse::<f64>()
                    .map(|v| (v - n).abs() < f64::EPSILON)
                    .unwrap_or(false)
            } else if let Some(b) = actual_val.as_bool() {
                expr.value.parse::<bool>().map(|v| v == b).unwrap_or(false)
            } else {
                actual_val == expr.value
            }
        }
        crate::parser::FilterOp::NotEqual => {
            if let Some(s) = actual_val.as_str() {
                s != expr.value
            } else if let Some(n) = actual_val.as_f64() {
                expr.value
                    .parse::<f64>()
                    .map(|v| (v - n).abs() >= f64::EPSILON)
                    .unwrap_or(true)
            } else if let Some(b) = actual_val.as_bool() {
                expr.value.parse::<bool>().map(|v| v != b).unwrap_or(true)
            } else {
                actual_val != expr.value
            }
        }
        crate::parser::FilterOp::GreaterThan => {
            if let (Some(actual), Ok(target)) = (actual_val.as_f64(), expr.value.parse::<f64>()) {
                actual > target
            } else if let Some(s) = actual_val.as_str() {
                s > expr.value.as_str()
            } else {
                false
            }
        }
        crate::parser::FilterOp::LessThan => {
            if let (Some(actual), Ok(target)) = (actual_val.as_f64(), expr.value.parse::<f64>()) {
                actual < target
            } else if let Some(s) = actual_val.as_str() {
                s < expr.value.as_str()
            } else {
                false
            }
        }
        crate::parser::FilterOp::GreaterOrEqual => {
            if let (Some(actual), Ok(target)) = (actual_val.as_f64(), expr.value.parse::<f64>()) {
                actual >= target
            } else if let Some(s) = actual_val.as_str() {
                s >= expr.value.as_str()
            } else {
                false
            }
        }
        crate::parser::FilterOp::LessOrEqual => {
            if let (Some(actual), Ok(target)) = (actual_val.as_f64(), expr.value.parse::<f64>()) {
                actual <= target
            } else if let Some(s) = actual_val.as_str() {
                s <= expr.value.as_str()
            } else {
                false
            }
        }
    }
}

fn render_structured_output(
    val: &serde_json::Value,
    format: Option<crate::parser::OutputFormat>,
) {
    use crate::builtins::{builtin_print, builtin_println};
    match format {
        Some(crate::parser::OutputFormat::Json) => {
            if let Ok(s) = serde_json::to_string_pretty(val) {
                builtin_println(&s);
            }
        }
        Some(crate::parser::OutputFormat::Yaml) => {
            if let Ok(s) = serde_yaml::to_string(val) {
                builtin_print(&s);
            }
        }
        Some(crate::parser::OutputFormat::Toml) => {
            if let Ok(s) = toml::to_string_pretty(val) {
                builtin_print(&s);
            }
        }
        Some(crate::parser::OutputFormat::Table) => {
            render_ascii_table(val);
        }
        None => match val {
            serde_json::Value::String(s) => builtin_println(s),
            serde_json::Value::Number(n) => builtin_println(&n.to_string()),
            serde_json::Value::Bool(b) => builtin_println(&b.to_string()),
            serde_json::Value::Null => {}
            serde_json::Value::Array(arr) => {
                let all_primitives = arr.iter().all(|item| {
                    matches!(
                        item,
                        serde_json::Value::String(_)
                            | serde_json::Value::Number(_)
                            | serde_json::Value::Bool(_)
                    )
                });
                if all_primitives {
                    for item in arr {
                        match item {
                            serde_json::Value::String(s) => builtin_println(s),
                            serde_json::Value::Number(n) => builtin_println(&n.to_string()),
                            serde_json::Value::Bool(b) => builtin_println(&b.to_string()),
                            _ => {}
                        }
                    }
                } else if let Ok(s) = serde_json::to_string_pretty(val) {
                    builtin_println(&s);
                }
            }
            serde_json::Value::Object(_) => {
                if let Ok(s) = serde_json::to_string_pretty(val) {
                    builtin_println(&s);
                }
            }
        },
    }
}

fn render_ascii_table(val: &serde_json::Value) {
    use crate::builtins::builtin_println;
    if let serde_json::Value::Array(arr) = val {
        if arr.is_empty() {
            return;
        }
        let mut keys = Vec::new();
        for item in arr {
            if let serde_json::Value::Object(map) = item {
                for k in map.keys() {
                    if !keys.contains(k) {
                        keys.push(k.clone());
                    }
                }
            }
        }
        if keys.is_empty() {
            return;
        }
        builtin_println(&keys.join("\t"));
        for item in arr {
            let row: Vec<String> = keys
                .iter()
                .map(|k| {
                    item.get(k)
                        .map(|v| match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .unwrap_or_default()
                })
                .collect();
            builtin_println(&row.join("\t"));
        }
    } else {
        builtin_println(&val.to_string());
    }
}

fn format_ast(ast: &Ast) -> String {
    match ast {
        Ast::Pipeline(p) => format_pipeline(p),
        Ast::If(if_stmt) => {
            format!("if {} then ... fi", format_pipeline(&if_stmt.condition))
        }
        Ast::For(parsed_for) => {
            format!("for {} in ...", parsed_for.variable)
        }
        Ast::While(_) => {
            "while ...".into()
        }
        Ast::Function(func) => {
            format!("{}() {{ ... }}", func.name)
        }
        Ast::StructuredPipeline(sp) => {
            format!("{} |> ...", format_ast(&sp.source))
        }
        Ast::Sequence(seq) => {
            let mut parts = Vec::new();
            for item in &seq.items {
                let node_str = match &item.node {
                    Ast::Pipeline(p) => format_pipeline(p),
                    Ast::If(i) => format!("if {} then ... fi", format_pipeline(&i.condition)),
                    Ast::For(f) => format!("for {} in ...", f.variable),
                    Ast::While(_) => "while ...".into(),
                    Ast::Function(f) => format!("{}() {{ ... }}", f.name),
                    Ast::StructuredPipeline(sp) => format!("{} |> ...", format_ast(&sp.source)),
                    Ast::Sequence(_) => "sequence".into(),
                };
                if let Some(ref op) = item.op {
                    match op {
                        LogicalOp::And => parts.push(format!("&& {node_str}")),
                        LogicalOp::Or => parts.push(format!("|| {node_str}")),
                    }
                } else if parts.is_empty() {
                    parts.push(node_str);
                } else {
                    parts.push(format!("; {node_str}"));
                }
            }
            parts.join(" ")
        }
    }
}

fn format_pipeline(pipeline: &ParsedPipeline) -> String {
    let base = pipeline
        .commands
        .iter()
        .map(format_command)
        .collect::<Vec<_>>()
        .join(" | ");
    if pipeline.background {
        format!("{base} &")
    } else {
        base
    }
}


enum PipelinePidWait {
    Completed(Vec<ExitStatus>),
    Stopped,
}

fn wait_for_pipeline_ids(process_ids: &[u32]) -> Result<PipelinePidWait, std::io::Error> {
    let mut statuses = Vec::with_capacity(process_ids.len());
    for process_id in process_ids {
        match wait_for_process(*process_id)? {
            WaitOutcome::Exited(status) => statuses.push(status),
            WaitOutcome::Stopped => return Ok(PipelinePidWait::Stopped),
        }
    }
    Ok(PipelinePidWait::Completed(statuses))
}

fn wait_for_process(pid: u32) -> Result<WaitOutcome, std::io::Error> {
    let mut status = 0;
    let result = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WUNTRACED) };
    if result == -1 {
        return Err(std::io::Error::last_os_error());
    }
    if libc::WIFSTOPPED(status) {
        Ok(WaitOutcome::Stopped)
    } else {
        Ok(WaitOutcome::Exited(ExitStatus::from_raw(status)))
    }
}

fn next_job_id(next_job_id: &RefCell<usize>) -> usize {
    let id = *next_job_id.borrow();
    *next_job_id.borrow_mut() += 1;
    id
}

fn run_in_foreground_group<T>(process_group: Option<u32>, operation: impl FnOnce() -> T) -> T {
    let Some(process_group) = process_group else {
        return operation();
    };
    let terminal = libc::STDIN_FILENO;
    if unsafe { libc::isatty(terminal) != 1 } {
        return operation();
    }
    let shell_group = unsafe { libc::tcgetpgrp(terminal) };
    if shell_group == -1 || shell_group != unsafe { libc::getpgrp() } {
        return operation();
    }
    let child_group = process_group as libc::pid_t;
    if unsafe { libc::tcsetpgrp(terminal, child_group) } == -1 {
        return operation();
    }
    let result = operation();
    let _ = unsafe { libc::tcsetpgrp(terminal, shell_group) };
    result
}

pub fn find_executable_in_path(command: &str) -> Option<PathBuf> {
    if command.is_empty() {
        return None;
    }

    let candidate = PathBuf::from(command);
    if command.starts_with('/')
        || command.starts_with("./")
        || command.starts_with("../")
        || candidate.components().count() > 1
    {
        return is_executable_file(&candidate).map(|path| path.to_path_buf());
    }

    let paths = env::var_os("PATH")?;

    for path in env::split_paths(&paths) {
        let full_path = path.join(command);

        if is_executable_file(&full_path).is_some() {
            return Some(full_path);
        }
    }

    None
}

fn is_executable_file(path: &PathBuf) -> Option<&PathBuf> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return None,
    };

    if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
        Some(path)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_pipeline;

    #[test]
    fn resolves_absolute_executables() {
        assert!(find_executable_in_path("/bin/ls").is_some());
    }

    #[test]
    fn missing_external_command_returns_status_127() {
        let executor = Executor::new();
        let pipeline = parse_pipeline("melchior-command-that-does-not-exist")
            .unwrap()
            .unwrap();

        let result = executor.execute_pipeline(&pipeline);

        assert_eq!(result.status_code(), 127);
    }

    #[test]
    fn missing_pipeline_command_returns_status_127() {
        let executor = Executor::new();
        let pipeline = parse_pipeline("/bin/true | melchior-command-that-does-not-exist")
            .unwrap()
            .unwrap();

        let result = executor.execute_pipeline(&pipeline);

        assert_eq!(result.status_code(), 127);
    }

    #[test]
    fn records_pipeline_history() {
        let executor = Executor::new();
        let pipeline = parse_pipeline("echo hello | wc -c").unwrap().unwrap();

        executor.record_history(&pipeline);

        assert_eq!(
            executor.history.borrow().as_slice(),
            &["echo hello | wc -c".to_string()]
        );
    }

    #[test]
    fn starts_background_external_pipelines() {
        let executor = Executor::new();
        let pipeline = parse_pipeline("/usr/bin/printf hello | cat &")
            .unwrap()
            .unwrap();

        let result = executor.execute_pipeline(&pipeline);

        assert!(matches!(result, ExecutionResult::Background));
        assert_eq!(executor.timeline()[0].stages.len(), 2);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(matches!(
            executor.execute(&ParsedCommand {
                program: "jobs".into(),
                args: Vec::new(),
                environment: Vec::new(),
                redirects: Vec::new(),
            }),
            ExecutionResult::Builtin
        ));
    }

    #[test]
    fn records_background_standalone_process_metadata() {
        let executor = Executor::new();
        let pipeline = parse_pipeline("/bin/true &").unwrap().unwrap();

        assert!(matches!(
            executor.execute_pipeline(&pipeline),
            ExecutionResult::Background
        ));
        assert_eq!(executor.timeline()[0].stages.len(), 1);
        assert!(executor.timeline()[0].stages[0].process_id.is_some());
    }

    #[test]
    fn applies_per_command_environment_assignments() {
        let executor = Executor::new();
        let pipeline = parse_pipeline("MELCHIOR_LOCAL=value printenv MELCHIOR_LOCAL")
            .unwrap()
            .unwrap();

        let result = executor.execute_pipeline(&pipeline);

        assert_eq!(result.status_code(), 0);
        assert!(std::env::var("MELCHIOR_LOCAL").is_err());
    }

    #[test]
    fn env_applies_assignments_to_a_command_without_leaking_them() {
        let executor = Executor::new();
        unsafe { env::remove_var("MELCHIOR_ENV_COMMAND") };
        let pipeline =
            parse_pipeline("env MELCHIOR_ENV_COMMAND=enabled printenv MELCHIOR_ENV_COMMAND")
                .unwrap()
                .unwrap();

        let result = executor.execute_pipeline(&pipeline);

        assert_eq!(result.status_code(), 0);
        assert!(env::var("MELCHIOR_ENV_COMMAND").is_err());
    }

    #[test]
    fn env_rejects_invalid_assignment_names() {
        let executor = Executor::new();
        let pipeline = parse_pipeline("env invalid-name=value true")
            .unwrap()
            .unwrap();

        assert_eq!(executor.execute_pipeline(&pipeline).status_code(), 1);
    }

    #[test]
    fn records_per_command_environment_keys() {
        let executor = Executor::new();
        let pipeline = parse_pipeline("MELCHIOR_RECORDED=value /bin/true")
            .unwrap()
            .unwrap();

        executor.execute_pipeline(&pipeline);

        assert!(
            executor.timeline()[0]
                .environment_keys
                .iter()
                .any(|key| key == "MELCHIOR_RECORDED")
        );
        assert!(std::env::var("MELCHIOR_RECORDED").is_err());
    }

    #[test]
    fn applies_and_restores_per_command_environment_for_builtins() {
        let executor = Executor::new();
        let name = "MELCHIOR_BUILTIN_LOCAL";
        unsafe { std::env::remove_var(name) };
        let command = ParsedCommand {
            program: "export".into(),
            args: vec!["MELCHIOR_BUILTIN_EXPORTED=value".into()],
            environment: vec![(name.into(), "scoped".into())],
            redirects: Vec::new(),
        };

        assert_eq!(executor.execute(&command).status_code(), 0);
        assert_eq!(std::env::var("MELCHIOR_BUILTIN_EXPORTED").unwrap(), "value");
        assert!(std::env::var(name).is_err());
        unsafe { std::env::remove_var("MELCHIOR_BUILTIN_EXPORTED") };
    }

    #[test]
    fn waits_for_a_background_job() {
        let executor = Executor::new();
        let pipeline = parse_pipeline("/bin/true &").unwrap().unwrap();
        executor.execute_pipeline(&pipeline);

        let result = executor.execute(&ParsedCommand {
            program: "wait".into(),
            args: Vec::new(),
            environment: Vec::new(),
            redirects: Vec::new(),
        });

        assert_eq!(result.status_code(), 0);
        assert!(executor.jobs.borrow().is_empty());
    }

    #[test]
    fn wait_keeps_unrequested_jobs_tracked() {
        let executor = Executor::new();
        let first = parse_pipeline("sleep 0.02 &").unwrap().unwrap();
        let second = parse_pipeline("sleep 0.02 &").unwrap().unwrap();
        executor.execute_pipeline(&first);
        executor.execute_pipeline(&second);

        let result = executor.execute(&ParsedCommand {
            program: "wait".into(),
            args: vec!["%1".into()],
            environment: Vec::new(),
            redirects: Vec::new(),
        });

        assert_eq!(result.status_code(), 0);
        assert_eq!(executor.jobs.borrow().len(), 1);
    }

    #[test]
    fn kills_a_tracked_background_job() {
        let executor = Executor::new();
        let pipeline = parse_pipeline("sleep 5 &").unwrap().unwrap();
        executor.execute_pipeline(&pipeline);

        let result = executor.execute(&ParsedCommand {
            program: "kill".into(),
            args: vec!["%1".into()],
            environment: Vec::new(),
            redirects: Vec::new(),
        });

        assert!(matches!(result, ExecutionResult::Builtin));
        let _ = executor.execute(&ParsedCommand {
            program: "wait".into(),
            args: vec!["%1".into()],
            environment: Vec::new(),
            redirects: Vec::new(),
        });
        assert!(executor.jobs.borrow().is_empty());
    }

    #[test]
    fn records_execution_metadata() {
        let executor = Executor::new();
        let pipeline = parse_pipeline("/bin/true").unwrap().unwrap();

        let result = executor.execute_pipeline(&pipeline);
        let record = executor.timeline().pop().unwrap();

        assert_eq!(result.status_code(), 0);
        assert_eq!(record.command, "/bin/true");
        assert_eq!(record.status, 0);
    }

    #[test]
    fn reloads_persisted_timeline_records() {
        let path =
            std::env::temp_dir().join(format!("melchior-timeline-{}.db", std::process::id()));
        let path_string = path.to_string_lossy().into_owned();
        unsafe { std::env::set_var("MELCHIOR_HISTORY_DB", &path_string) };

        {
            let executor = Executor::persistent();
            let pipeline = parse_pipeline("/bin/true").unwrap().unwrap();
            executor.execute_pipeline(&pipeline);
            assert_eq!(executor.timeline().len(), 1);
        }

        let restored = Executor::persistent();
        let record = restored
            .timeline()
            .pop()
            .expect("persisted timeline record");
        assert_eq!(record.command, "/bin/true");
        assert_eq!(record.status, 0);
        assert_eq!(record.audit.outcome, AuditOutcome::Executed);

        let _ = std::fs::remove_file(path);
        unsafe { std::env::remove_var("MELCHIOR_HISTORY_DB") };
        assert!(!record.working_directory.as_os_str().is_empty());
        assert!(record.environment_keys.iter().any(|key| key == "HOME"));
        assert!(record.redirections.is_empty());
        assert_eq!(record.stages[0].program, "/bin/true");
        assert_eq!(
            record.audit,
            AuditEvent {
                decision: PolicyDecision::Allow,
                outcome: AuditOutcome::Executed
            }
        );
        assert!(
            record
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::ProcessCreation))
        );
    }

    #[test]
    fn records_redirection_metadata() {
        let executor = Executor::new();
        let pipeline = parse_pipeline("/bin/true > output.log").unwrap().unwrap();

        executor.execute_pipeline(&pipeline);

        assert_eq!(
            executor.timeline()[0].redirections,
            vec![Redirection::Stdout("output.log".into())]
        );
        assert_eq!(executor.timeline()[0].policy, PolicyDecision::Ask);
        assert_eq!(executor.timeline()[0].audit.outcome, AuditOutcome::Denied);
    }

    #[test]
    fn redirects_builtin_output_without_leaking_descriptor_state() {
        let executor = Executor::new();
        let path =
            std::env::temp_dir().join(format!("melchior-builtin-{}.log", std::process::id()));
        let result = executor.execute(&ParsedCommand {
            program: "echo".into(),
            args: vec!["redirected".into()],
            environment: Vec::new(),
            redirects: vec![Redirection::Stdout(path.to_string_lossy().into_owned())],
        });

        assert!(matches!(result, ExecutionResult::Builtin));
        assert!(path.exists());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn records_standalone_external_process_id() {
        let executor = Executor::new();
        let pipeline = parse_pipeline("/bin/true").unwrap().unwrap();

        executor.execute_pipeline(&pipeline);

        let record = executor.timeline().pop().unwrap();
        assert!(record.stages[0].process_id.is_some());
    }

    #[test]
    fn records_blocked_policy_events() {
        let executor = Executor::new();
        let pipeline = parse_pipeline("sudo true").unwrap().unwrap();

        executor.execute_pipeline(&pipeline);

        let record = executor.timeline().pop().unwrap();
        assert_eq!(record.audit.decision, PolicyDecision::Block);
        assert_eq!(record.audit.outcome, AuditOutcome::Blocked);
    }

    #[test]
    fn processes_builtin_rejects_arguments() {
        assert!(matches!(
            list_processes(&["unexpected".into()]),
            ExecutionResult::Failed
        ));
    }

    #[test]
    fn decodes_proc_network_endpoints() {
        assert_eq!(
            decode_proc_endpoint("0100007F:0016", false).unwrap(),
            "127.0.0.1:22"
        );
        assert_eq!(
            parse_proc_socket(
                "  0: 0100007F:0016 00000000:0000 0A 00000000:0000 00000000:00000000 00000000   1 0 12345 1 0000000000000000 100 0 0 10 0",
                false
            )
            .unwrap()
            .0,
            "LISTEN"
        );
    }

    #[test]
    fn ports_builtin_rejects_arguments() {
        assert!(matches!(
            list_ports(&["unexpected".into()]),
            ExecutionResult::Failed
        ));
    }

    #[test]
    fn connections_builtin_rejects_arguments() {
        assert!(matches!(
            list_connections(&["unexpected".into()]),
            ExecutionResult::Failed
        ));
    }

    #[test]
    fn children_builtin_validates_pid() {
        assert!(matches!(
            list_children(&["not-a-pid".into()]),
            ExecutionResult::Failed
        ));
        assert!(matches!(list_children(&[]), ExecutionResult::Builtin));
    }

    #[test]
    fn missing_process_is_not_reported_as_stopped() {
        assert!(!is_process_stopped(u32::MAX));
    }

    #[test]
    fn detects_and_reaps_a_stopped_child() {
        let mut process = Command::new("/bin/sh");
        process.args(["-c", "kill -STOP $$"]);
        reset_child_signals(&mut process, None);
        let mut child = process.spawn().expect("spawn stopped test child");
        let pid = child.id();

        assert!(matches!(wait_for_process(pid), Ok(WaitOutcome::Stopped)));
        unsafe {
            assert_eq!(libc::kill(pid as libc::pid_t, libc::SIGCONT), 0);
        }
        let status = child.wait().expect("wait for child");
        assert!(status.success());
    }

    #[test]
    fn json_builtin_validates_and_formats_values() {
        assert!(matches!(
            format_json(&["{\"ok\":true}".into()]),
            ExecutionResult::Builtin
        ));
        assert!(matches!(
            format_json(&["not-json".into()]),
            ExecutionResult::Failed
        ));
        assert!(matches!(format_json(&[]), ExecutionResult::Failed));
    }

    #[test]
    fn yaml_builtin_validates_and_formats_values() {
        assert!(matches!(
            format_yaml(&["name: melchior".into()]),
            ExecutionResult::Builtin
        ));
        assert!(matches!(
            format_yaml(&["name: [".into()]),
            ExecutionResult::Failed
        ));
        assert!(matches!(format_yaml(&[]), ExecutionResult::Failed));
    }

    #[test]
    fn toml_builtin_validates_and_formats_values() {
        assert!(matches!(
            format_toml(&["name = \"melchior\"".into()]),
            ExecutionResult::Builtin
        ));
        assert!(matches!(
            format_toml(&["name =".into()]),
            ExecutionResult::Failed
        ));
        assert!(matches!(format_toml(&[]), ExecutionResult::Failed));
    }

    #[test]
    fn help_builtin_validates_arguments() {
        assert!(matches!(show_help(&[]), ExecutionResult::Builtin));
        assert!(matches!(
            show_help(&["unexpected".into()]),
            ExecutionResult::Failed
        ));
    }

    #[test]
    fn executes_builtin_in_pipeline() {
        let executor = Executor::new();
        let ast = crate::parser::parse_ast("echo hello | grep hello", 0)
            .unwrap()
            .unwrap();
        let result = executor.execute_ast(&ast);
        assert_eq!(result.status_code(), 0);

        let ast_fail = crate::parser::parse_ast("echo hello | grep world", 0)
            .unwrap()
            .unwrap();
        let result_fail = executor.execute_ast(&ast_fail);
        assert_eq!(result_fail.status_code(), 1);
    }

    #[test]
    fn executes_logical_sequences() {
        let executor = Executor::new();
        let ast_and = crate::parser::parse_ast("true && echo ok", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast_and).status_code(), 0);

        let ast_short = crate::parser::parse_ast("false && echo should_not_run", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast_short).status_code(), 1);

        let ast_or = crate::parser::parse_ast("false || true", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast_or).status_code(), 0);

        let ast_or_short = crate::parser::parse_ast("true || false", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast_or_short).status_code(), 0);
    }

    #[test]
    fn executes_capture_subshell() {
        let output = execute_capture("echo hello world", 0).unwrap();
        assert_eq!(output, "hello world");
    }

    #[test]
    fn executes_here_string_redirection() {
        let executor = Executor::new();
        let ast = crate::parser::parse_ast("cat <<< hello", 0).unwrap().unwrap();
        let result = executor.execute_ast(&ast);
        assert_eq!(result.status_code(), 0);
    }

    #[test]
    fn abbr_builtin_manages_abbreviations() {
        let executor = Executor::new();
        assert_eq!(executor.abbreviations().get("gco").unwrap(), "git checkout");

        let ast_add = crate::parser::parse_ast("abbr add k kubectl", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast_add).status_code(), 0);
        assert_eq!(executor.abbreviations().get("k").unwrap(), "kubectl");

        let ast_rm = crate::parser::parse_ast("abbr remove k", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast_rm).status_code(), 0);
        assert!(!executor.abbreviations().contains_key("k"));
    }

    #[test]
    fn structured_pipeline_evaluates_projections_and_filters() {
        let json_data = r#"[
            {"name": "svc-a", "status": "running", "cpu": 12.5},
            {"name": "svc-b", "status": "stopped", "cpu": 0.0},
            {"name": "svc-c", "status": "running", "cpu": 45.2}
        ]"#;
        let parsed = parse_structured_data(json_data).unwrap();

        // 1. Filter status == "running"
        let filter_status = crate::parser::FilterExpr {
            field: "status".into(),
            op: crate::parser::FilterOp::Equal,
            value: "running".into(),
        };
        let filtered = filter_structured_value(&parsed, &filter_status);

        // 2. Project .name
        let projected = project_structured_value(&filtered, ".name");
        assert_eq!(projected, serde_json::json!(["svc-a", "svc-c"]));

        // 3. Filter cpu > 20.0
        let filter_cpu = crate::parser::FilterExpr {
            field: "cpu".into(),
            op: crate::parser::FilterOp::GreaterThan,
            value: "20.0".into(),
        };
        let filtered_cpu = filter_structured_value(&parsed, &filter_cpu);
        let projected_cpu = project_structured_value(&filtered_cpu, ".name");
        assert_eq!(projected_cpu, serde_json::json!(["svc-c"]));
    }

    #[test]
    fn structured_pipeline_parses_yaml_and_jsonl() {
        // JSONL
        let jsonl = "{\"id\": 10}\n{\"id\": 20}\n{\"id\": 30}\n";
        let parsed_jsonl = parse_structured_data(jsonl).unwrap();
        let projected = project_structured_value(&parsed_jsonl, ".id");
        assert_eq!(projected, serde_json::json!([10, 20, 30]));

        // YAML
        let yaml = "service:\n  name: melchior\n  port: 8080\n";
        let parsed_yaml = parse_structured_data(yaml).unwrap();
        let projected_name = project_structured_value(&parsed_yaml, ".service.name");
        assert_eq!(projected_name, serde_json::json!("melchior"));
        let projected_port = project_structured_value(&parsed_yaml, ".service.port");
        assert_eq!(projected_port, serde_json::json!(8080));
    }

    #[test]
    fn executes_structured_pipeline_end_to_end() {
        let executor = Executor::new();
        let ast = crate::parser::parse_ast(
            r#"echo '[{"id": 1, "active": true}, {"id": 2, "active": false}]' |> filter (.active == true) |> .id"#,
            0,
        )
        .unwrap()
        .unwrap();

        let result = executor.execute_ast(&ast);
        assert_eq!(result.status_code(), 0);
    }

    #[test]
    fn executes_structured_pipeline_take_count_and_format() {
        let executor = Executor::new();
        let ast = crate::parser::parse_ast(
            r#"echo '[{"x": 1}, {"x": 2}, {"x": 3}, {"x": 4}]' |> take 2 |> count"#,
            0,
        )
        .unwrap()
        .unwrap();

        let result = executor.execute_ast(&ast);
        assert_eq!(result.status_code(), 0);

        let ast_yaml = crate::parser::parse_ast(
            r#"echo '{"cluster": "prod", "nodes": 3}' |> yaml"#,
            0,
        )
        .unwrap()
        .unwrap();
        let result_yaml = executor.execute_ast(&ast_yaml);
        assert_eq!(result_yaml.status_code(), 0);
    }

    #[test]
    fn executes_for_loop_and_updates_variable() {
        let executor = Executor::new();
        let ast = crate::parser::parse_ast(
            "for item in apple banana; do export LAST_FRUIT=$item; done",
            0,
        )
        .unwrap()
        .unwrap();

        let result = executor.execute_ast(&ast);
        assert_eq!(result.status_code(), 0);
        assert_eq!(std::env::var("LAST_FRUIT").unwrap(), "banana");
        unsafe { std::env::remove_var("LAST_FRUIT") };
    }

    #[test]
    fn executes_while_loop_with_dynamic_condition() {
        let executor = Executor::new();
        let ast = crate::parser::parse_ast(
            "export COUNTER=1; while [ $COUNTER = 1 ]; do export COUNTER=0; done",
            0,
        )
        .unwrap()
        .unwrap();

        let result = executor.execute_ast(&ast);
        assert_eq!(result.status_code(), 0);
        assert_eq!(std::env::var("COUNTER").unwrap(), "0");
        unsafe { std::env::remove_var("COUNTER") };
    }

    #[test]
    fn executes_standalone_environment_assignment() {
        let executor = Executor::new();
        let ast = crate::parser::parse_ast("ASSIGNED_VAR=configured", 0)
            .unwrap()
            .unwrap();

        let result = executor.execute_ast(&ast);
        assert_eq!(result.status_code(), 0);
        assert_eq!(std::env::var("ASSIGNED_VAR").unwrap(), "configured");
        unsafe { std::env::remove_var("ASSIGNED_VAR") };
    }

    #[test]
    fn executes_functions_with_arguments_and_unset() {
        let executor = Executor::new();
        let def_ast = crate::parser::parse_ast("set_var() { export FN_ARG=$1; }", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&def_ast).status_code(), 0);

        assert!(executor.functions().contains_key("set_var"));

        let call_ast = crate::parser::parse_ast("set_var melchior_val", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&call_ast).status_code(), 0);
        assert_eq!(std::env::var("FN_ARG").unwrap(), "melchior_val");
        unsafe { std::env::remove_var("FN_ARG") };

        // Test type builtin recognizing the function
        let type_ast = crate::parser::parse_ast("type set_var", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&type_ast).status_code(), 0);

        // Test unset -f removes function
        let unset_ast = crate::parser::parse_ast("unset -f set_var", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&unset_ast).status_code(), 0);
        assert!(!executor.functions().contains_key("set_var"));
    }

    #[test]
    fn manages_and_expands_aliases() {
        let executor = Executor::new();
        let alias_ast = crate::parser::parse_ast("alias my_echo=\"export MY_VAR=aliased\"", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&alias_ast).status_code(), 0);
        assert_eq!(executor.aliases().get("my_echo").unwrap(), "export MY_VAR=aliased");

        // Test type builtin recognizing the alias
        let type_ast = crate::parser::parse_ast("type my_echo", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&type_ast).status_code(), 0);

        // Execute using alias expansion
        let run_ast = crate::parser::parse_ast("my_echo", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&run_ast).status_code(), 0);
        assert_eq!(std::env::var("MY_VAR").unwrap(), "aliased");
        unsafe { std::env::remove_var("MY_VAR") };

        // Unalias single
        let unalias_ast = crate::parser::parse_ast("unalias my_echo", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&unalias_ast).status_code(), 0);
        assert!(!executor.aliases().contains_key("my_echo"));

        // Unalias -a
        executor.set_alias("a1".into(), "val1".into());
        executor.set_alias("a2".into(), "val2".into());
        let unalias_all = crate::parser::parse_ast("unalias -a", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&unalias_all).status_code(), 0);
        assert!(executor.aliases().is_empty());
    }

    #[test]
    fn filters_history_and_timeline() {
        let executor = Executor::new();
        let ast1 = crate::parser::parse_ast("echo test1", 0).unwrap().unwrap();
        executor.execute_ast(&ast1);

        let ast2 = crate::parser::parse_ast("history dir .", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast2).status_code(), 0);

        let ast3 = crate::parser::parse_ast("history status 0", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast3).status_code(), 0);

        let ast4 = crate::parser::parse_ast("history --failed", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast4).status_code(), 0);

        let ast5 = crate::parser::parse_ast("timeline dir .", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast5).status_code(), 0);

        let ast6 = crate::parser::parse_ast("timeline status 0", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast6).status_code(), 0);
    }

    #[test]
    fn tutor_command_lifecycle_in_executor() {
        let executor = Executor::new();

        // 1. tutor list
        let ast_list = crate::parser::parse_ast("tutor list", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast_list).status_code(), 0);

        // 2. tutor start nav_01
        let ast_start = crate::parser::parse_ast("tutor start nav_01", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast_start).status_code(), 0);
        assert_eq!(
            executor.tutor.borrow().as_ref().unwrap().active_lesson_id.as_deref(),
            Some("nav_01")
        );

        // 3. tutor hint
        let ast_hint = crate::parser::parse_ast("tutor hint", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast_hint).status_code(), 0);

        // 4. tutor solution
        let ast_sol = crate::parser::parse_ast("tutor solution", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast_sol).status_code(), 0);

        // 5. tutor check (incomplete initially)
        let ast_check = crate::parser::parse_ast("tutor check", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast_check).status_code(), 1);

        // 6. Complete nav_01 in sandbox workspace
        let ws = executor.current_workspace();
        std::fs::create_dir_all(ws.join("docs")).unwrap();
        std::fs::copy(ws.join("README.md"), ws.join("docs").join("overview.txt")).unwrap();

        // 7. tutor check should now succeed and mark nav_01 complete
        assert_eq!(executor.execute_ast(&ast_check).status_code(), 0);

        // 8. tutor next advances to nav_02 without RefCell borrow panic
        let ast_next = crate::parser::parse_ast("tutor next", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast_next).status_code(), 0);
        assert_eq!(
            executor.tutor.borrow().as_ref().unwrap().active_lesson_id.as_deref(),
            Some("nav_02")
        );

        // 9. Solve nav_02: mkdir -p services/payment/handlers/v2 services/payment/tests
        std::fs::create_dir_all(ws.join("services").join("payment").join("handlers").join("v2")).unwrap();
        std::fs::create_dir_all(ws.join("services").join("payment").join("tests")).unwrap();
        assert_eq!(executor.execute_ast(&ast_check).status_code(), 0);

        // 10. tutor next advances to nav_03
        assert_eq!(executor.execute_ast(&ast_next).status_code(), 0);
        assert_eq!(
            executor.tutor.borrow().as_ref().unwrap().active_lesson_id.as_deref(),
            Some("nav_03")
        );

        // 11. tutor reset
        let ast_reset = crate::parser::parse_ast("tutor reset", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast_reset).status_code(), 0);

        // 12. tutor exit
        let ast_exit = crate::parser::parse_ast("tutor exit", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast_exit).status_code(), 0);
        assert!(executor.tutor.borrow().as_ref().unwrap().active_lesson_id.is_none());
    }

    #[test]
    fn whatif_and_undo_and_snapshot_in_executor() {
        let executor = Executor::new();

        // 1. whatif command
        let ast_whatif = crate::parser::parse_ast("whatif rm test.txt", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast_whatif).status_code(), 0);

        // 2. snapshot command initializes sandbox and creates checkpoint
        let ast_snap = crate::parser::parse_ast("snapshot checkpoint1", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast_snap).status_code(), 0);
        assert!(executor.sandbox.borrow().is_some());

        // 3. undo command
        let ast_undo = crate::parser::parse_ast("undo", 0).unwrap().unwrap();
        assert_eq!(executor.execute_ast(&ast_undo).status_code(), 0);
    }

    #[test]
    fn tutor_nav_02_with_brace_expansion_works() {
        let original_dir = std::env::current_dir().unwrap();
        let executor = Executor::new();
        executor.set_interactive(true);
        let ws = executor.ensure_sandbox().unwrap();
        let start_res = executor.tutor_cmd(&["start".into(), "nav_02".into()]);
        assert_eq!(start_res.status_code(), 0);

        // Execute mkdir with brace expansion
        let ast = crate::parser::parse_ast("mkdir -p services/payment/{handlers/v2,tests}", 0)
            .unwrap()
            .unwrap();
        let exec_res = executor.execute_ast(&ast);
        assert_eq!(exec_res.status_code(), 0);

        // Verify tutor check succeeds
        let check_res = executor.tutor_cmd(&["check".into()]);
        assert_eq!(check_res.status_code(), 0);

        let d1 = ws.join("services").join("payment").join("handlers").join("v2");
        let d2 = ws.join("services").join("payment").join("tests");
        assert!(d1.is_dir(), "handlers/v2 directory must exist");
        assert!(d2.is_dir(), "tests directory must exist");

        let _ = std::env::set_current_dir(&original_dir);
    }
}
