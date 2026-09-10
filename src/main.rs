mod builtins;
mod completions;
mod effects;
mod executor;
mod explainer;
mod parser;
mod policy;
mod shell;

fn main() {
    if let Err(error) = shell::configure_signal_handling() {
        eprintln!("mshell: {error}");
        std::process::exit(1);
    }

    let args: Vec<String> = std::env::args().collect();
    let mut shell = shell::Shell::new();
    std::process::exit(shell.run_with_args(&args));
}
