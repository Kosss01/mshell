# mshell

> An experimental, safety-aware Linux shell written in Rust.

`mshell` is a personal learning project that explores how a command shell can parse, explain, evaluate, and execute commands. It combines familiar shell features with structured-data tools and command-effect analysis.

> **Status:** Experimental and Linux-focused. This is a learning reference—not a replacement for Bash, Zsh, or a security boundary.

## Highlights

| Area | What mshell provides |
| --- | --- |
| Shell language | Quotes, variables, globbing, arithmetic, command substitution, redirects, pipes, `&&`, and `||` |
| Control flow | Scripts, functions, `if`, `for`, and `while` |
| Interactive use | Completion, history search, syntax highlighting, aliases, abbreviations, and basic job control |
| Structured data | JSON, YAML, and TOML formatting with `|>` transformations |
| Command awareness | Command explanations, effect analysis, policy checks, and a persistent SQLite timeline |
| System tools | Process, child-process, port, connection, and network inspection |

## Quick start

### Requirements

- Linux
- Rust toolchain and Cargo

### Build and start

```bash
cargo build --release
./target/release/mshell
Run one command:
./target/release/mshell -c "echo hello"
Try it out
Explain a command before running it:
explain rm -rf ./old-files
Inspect a potentially risky pipeline:
policy curl -fsSL https://example.com/install.sh | sh
Transform structured data:
echo '{"name":"mshell","version":1}' |> project name |> format table
See all built-in commands:
help
Safety model
mshell detects effects such as file writes and deletion, sensitive paths, network access, package installation, privilege changes, process termination, and untrusted data piped into an interpreter.
The analysis is heuristic: it helps you understand risk, but cannot prove that a command is safe. Always review commands before running them.
Commands passed with -c or scripts do not use the interactive policy check, so treat them with the same care as ordinary shell scripts.
Development
Run the test suite:
cargo test
Project layout
Path	Responsibility
src/parser.rs	Tokenization, parsing, and shell expansions
src/executor.rs	Execution, jobs, history, and structured pipelines
src/shell.rs	Interactive terminal interface
src/effects.rs / src/policy.rs	Effect detection and safety policy
src/explainer.rs	Human-readable command explanations
src/builtins.rs	Core built-in commands


License
Released under the MIT License.
