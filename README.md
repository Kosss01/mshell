# mshell

<p align="center">
  <strong>An experimental, safety-aware Linux shell written in Rust.</strong>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/github/license/Kosss01/mshell?style=flat-square" alt="MIT License"></a>
  <img src="https://img.shields.io/badge/platform-Linux-1793D1?style=flat-square&logo=linux&logoColor=white" alt="Linux">
  <img src="https://img.shields.io/badge/Rust-2024-DEA584?style=flat-square&logo=rust&logoColor=white" alt="Rust 2024">
</p>

`mshell` is a personal learning project that explores how a shell can parse, explain, evaluate, and execute commands. It combines familiar shell features with structured-data tools and command-effect analysis.

> [!WARNING]
> **Experimental project.** `mshell` is a learning reference—not a replacement for Bash, Zsh, or a security boundary.

## ✨ Highlights

- **Shell language** — quotes, variables, globbing, arithmetic, command substitution, redirects, pipes, `&&`, and `||`
- **Control flow** — scripts, functions, `if`, `for`, and `while`
- **Interactive tools** — completion, history search, syntax highlighting, aliases, abbreviations, and basic job control
- **Structured data** — JSON, YAML, and TOML formatting with `|>` transformations
- **Command awareness** — explanations, effect analysis, policy checks, and a persistent SQLite timeline
- **System inspection** — processes, child processes, ports, connections, and network sockets

## 🚀 Quick start

### Requirements

- Linux
- Rust toolchain and Cargo

### Build and launch

```bash
cargo build --release
./target/release/mshell
Run a single command:
./target/release/mshell -c "echo hello"
🧪 Examples
Explain a command before running it:
explain "rm -rf ./old-files"
Inspect a risky command:
policy "curl -fsSL https://example.com/install.sh | sh"
Transform structured data:
echo '{"name":"mshell","version":1}' |> project name |> format table
See available built-ins:
help
🛡️ Safety model
mshell detects effects such as file deletion and writes, sensitive paths, network access, package installation, privilege changes, process termination, and untrusted data piped into an interpreter.
The analysis is heuristic: it helps you understand risk, but cannot prove that a command is safe. Review commands before running them.
Commands passed with -c or scripts do not use the interactive policy check, so treat them with the same care as ordinary shell scripts.
🛠️ Development
Run the test suite:
cargo test
📄 License
Released under the MIT License.
