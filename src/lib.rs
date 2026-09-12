pub mod builtins;
pub mod cadet;
pub mod completions;
pub mod drills;
pub mod effects;
pub mod executor;
pub mod explainer;
pub mod guidance;
pub mod linter;
pub mod parser;
pub mod policy;
pub mod sandbox;
pub mod server_sim;
pub mod shell;
pub mod tutor;

pub fn run() -> i32 {
    if let Err(error) = shell::configure_signal_handling() {
        eprintln!("shellpilot: {error}");
        return 1;
    }

    let args: Vec<String> = std::env::args().collect();
    let mut shell = shell::Shell::new();
    shell.run_with_args(&args)
}

