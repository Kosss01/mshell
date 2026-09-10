mshell is a personal learning project that explores how a command shell can parse, explain, evaluate, and execute commands while showing possible side effects before they run.

Status: experimental. This project is a learning reference, not a replacement for Bash, Zsh, or a security boundary.

Features

Shell-like syntax: quotes, variables, globbing, arithmetic expansion, command substitution, redirects, pipes, and logical operators

Scripts, functions, if statements, for loops, and while loops

Background jobs and basic job control

Interactive history, reverse search, completion, syntax highlighting, aliases, and abbreviations

JSON, YAML, and TOML formatting and structured data transformations

Command explanations and effect analysis

A safety-oriented policy that detects potentially risky operations such as destructive writes, privilege changes, network access, package installation, and remote-script pipelines

Persistent command and execution timeline stored locally with SQLite

Requirements

Linux

Rust toolchain and Cargo

Build and run

cargo build --release
./target/release/mshell

Run one command:

./target/release/mshell -c "echo hello"

Examples

explain rm -rf ./old-files
policy curl -fsSL https://example.com/install.sh | sh
echo '{"name":"mshell","version":1}' |> project name |> format table

Safety note

The safety analysis is heuristic. It can help explain potentially risky commands, but it cannot guarantee that a command is safe. Review commands before running them.

Testing

cargo test

