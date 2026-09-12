use std::fs;
use std::path::Path;

// ─────────────────────────────────────────────────────────────────────────────
// 1. TREE DIRECTORY VISUALIZER
// ─────────────────────────────────────────────────────────────────────────────

pub struct TreeOptions {
    pub max_depth: usize,
    pub dirs_only: bool,
}

impl Default for TreeOptions {
    fn default() -> Self {
        Self {
            max_depth: 4,
            dirs_only: false,
        }
    }
}

pub fn render_tree(root: &Path, opts: &TreeOptions) -> Result<String, String> {
    if !root.exists() {
        return Err(format!("tree: '{}': No such file or directory", root.display()));
    }

    let mut out = String::new();
    let display_name = if root == Path::new(".") {
        ".".to_string()
    } else {
        root.display().to_string()
    };
    out.push_str(&format!("\x1b[1;34m{}\x1b[0m\n", display_name));

    let mut dir_count = 0;
    let mut file_count = 0;

    render_subtree(root, "", 1, opts, &mut out, &mut dir_count, &mut file_count)?;

    let summary = if opts.dirs_only {
        format!("\n\x1b[0;90m{} {}\x1b[0m\n", dir_count, if dir_count == 1 { "directory" } else { "directories" })
    } else {
        format!(
            "\n\x1b[0;90m{} {}, {} {}\x1b[0m\n",
            dir_count,
            if dir_count == 1 { "directory" } else { "directories" },
            file_count,
            if file_count == 1 { "file" } else { "files" }
        )
    };
    out.push_str(&summary);

    Ok(out)
}

fn render_subtree(
    dir: &Path,
    prefix: &str,
    current_depth: usize,
    opts: &TreeOptions,
    out: &mut String,
    dir_count: &mut usize,
    file_count: &mut usize,
) -> Result<(), String> {
    if current_depth > opts.max_depth {
        return Ok(());
    }

    let mut entries = Vec::new();
    if let Ok(read_dir) = fs::read_dir(dir) {
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            // Skip hidden git folders
            if name == ".git" {
                continue;
            }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if opts.dirs_only && !is_dir {
                continue;
            }
            entries.push((name, entry.path(), is_dir));
        }
    }

    // Sort: directories first, then alphabetically
    entries.sort_by(|a, b| {
        b.2.cmp(&a.2).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
    });

    let total = entries.len();
    for (idx, (name, path, is_dir)) in entries.iter().enumerate() {
        let is_last = idx + 1 == total;
        let connector = if is_last { "└── " } else { "├── " };

        if *is_dir {
            *dir_count += 1;
            out.push_str(&format!("{}{}\x1b[1;34m{}\x1b[0m\n", prefix, connector, name));
            let new_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
            render_subtree(path, &new_prefix, current_depth + 1, opts, out, dir_count, file_count)?;
        } else {
            *file_count += 1;
            let formatted_name = if name.ends_with(".sh") || name.ends_with(".py") {
                format!("\x1b[1;32m{}\x1b[0m", name)
            } else if name.ends_with(".log") {
                format!("\x1b[0;33m{}\x1b[0m", name)
            } else if name.ends_with(".json") || name.ends_with(".yaml") || name.ends_with(".conf") {
                format!("\x1b[0;36m{}\x1b[0m", name)
            } else {
                name.clone()
            };
            out.push_str(&format!("{}{}{}\n", prefix, connector, formatted_name));
        }
    }

    Ok(())
}

pub fn run_tree(args: &[String]) -> Result<String, String> {
    let mut opts = TreeOptions::default();
    let mut target_dir = ".".to_string();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-d" => {
                opts.dirs_only = true;
            }
            "-L" => {
                if i + 1 < args.len() {
                    if let Ok(lvl) = args[i + 1].parse::<usize>() {
                        opts.max_depth = lvl.max(1);
                    }
                    i += 1;
                }
            }
            arg if !arg.starts_with('-') => {
                target_dir = arg.to_string();
            }
            _ => {}
        }
        i += 1;
    }

    render_tree(Path::new(&target_dir), &opts)
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. CHEAT INSTANT OFFLINE CHEATSHEETS
// ─────────────────────────────────────────────────────────────────────────────

pub struct CheatCard {
    pub name: &'static str,
    pub description: &'static str,
    pub flags: &'static [(&'static str, &'static str)],
    pub recipes: &'static [(&'static str, &'static str)],
}

pub const CHEAT_CARDS: &[CheatCard] = &[
    CheatCard {
        name: "grep",
        description: "Search text using regular expressions and string matching",
        flags: &[
            ("-i", "Case-insensitive search"),
            ("-v", "Invert match (select non-matching lines)"),
            ("-r, -R", "Recursive search in directories"),
            ("-n", "Show 1-based line numbers"),
            ("-c", "Count matching lines only"),
            ("-E", "Extended regex (ERE) syntax"),
        ],
        recipes: &[
            ("grep ' 404 ' logs/access.log", "Find all HTTP 404 error entries"),
            ("grep -rn 'DATABASE_URL' config/", "Recursively search for database credentials"),
            ("grep -i 'failed' logs/auth.log", "Case-insensitive search for authentication failures"),
            ("grep -v '^#' config/server.conf", "Filter out comment lines starting with '#'"),
        ],
    },
    CheatCard {
        name: "find",
        description: "Search for files in a directory hierarchy",
        flags: &[
            ("-name <pat>", "Match filename pattern (case-sensitive)"),
            ("-iname <pat>", "Match filename pattern (case-insensitive)"),
            ("-type f", "Match regular files only"),
            ("-type d", "Match directories only"),
            ("-size +10M", "Match files larger than 10 Megabytes"),
            ("-mtime -7", "Files modified within the last 7 days"),
        ],
        recipes: &[
            ("find . -name '*.bak'", "Locate all backup files in current tree"),
            ("find . -type f -name '*.sh' -exec chmod +x {} +", "Make all shell scripts executable"),
            ("find logs/ -size +50M", "Find oversized log files filling up disk"),
            ("find . -type d -name 'v2'", "Locate all directories named 'v2'"),
        ],
    },
    CheatCard {
        name: "chmod",
        description: "Change file access permissions and modes",
        flags: &[
            ("+x", "Add execute permission for all users"),
            ("-w", "Remove write permissions (make read-only)"),
            ("600", "Read & write for owner only (SSH private keys)"),
            ("644", "Owner read/write, world read-only (standard files)"),
            ("755", "Owner read/write/execute, world read/execute (scripts/dirs)"),
            ("-R", "Apply permissions recursively"),
        ],
        recipes: &[
            ("chmod +x scripts/deploy.sh", "Make deployment script runnable"),
            ("chmod 600 keys/deploy_key.pem", "Secure SSH private key (owner-only)"),
            ("chmod 444 config/database.yaml", "Protect database configuration as read-only"),
            ("chmod -R 755 services/", "Set standard directory & executable permissions"),
        ],
    },
    CheatCard {
        name: "tar",
        description: "Archive and compress files",
        flags: &[
            ("-c", "Create a new archive"),
            ("-x", "Extract files from an archive"),
            ("-z", "Filter the archive through gzip (.tar.gz)"),
            ("-v", "Verbosely list files processed"),
            ("-f", "Use archive file specified as next argument"),
        ],
        recipes: &[
            ("tar -czvf backups/app.tar.gz app/ config/", "Create compressed gzip archive of app and config"),
            ("tar -xzvf backups/app.tar.gz", "Extract a .tar.gz archive into current folder"),
            ("tar -tzvf backups/app.tar.gz", "List contents of an archive without extracting"),
        ],
    },
    CheatCard {
        name: "sed",
        description: "Stream editor for filtering and transforming text",
        flags: &[
            ("-i", "Edit files in-place (directly modifies file)"),
            ("-e", "Add script expression to be executed"),
            ("s/find/replace/g", "Substitute all occurrences of 'find' with 'replace'"),
        ],
        recipes: &[
            ("sed -i 's/8080/9000/g' config/server.conf", "Change server port from 8080 to 9000 in place"),
            ("sed '/^#/d' config/server.conf", "Delete all comment lines starting with '#'"),
            ("sed -i '1s/^/#!/bin/bash\\n/' script.sh", "Insert shebang line at top of file"),
        ],
    },
    CheatCard {
        name: "cut",
        description: "Remove sections from each line of files",
        flags: &[
            ("-d <delim>", "Use delimiter instead of TAB (e.g. -d',' or -d' ')"),
            ("-f <list>", "Select only these fields (e.g. -f1, -f2-4)"),
        ],
        recipes: &[
            ("cut -d',' -f3 data/customers.csv", "Extract the 3rd column (emails) from CSV"),
            ("cut -d' ' -f1 logs/access.log", "Extract client IP addresses from Apache/Nginx log"),
            ("cut -d':' -f1 /etc/passwd", "Extract all system usernames"),
        ],
    },
    CheatCard {
        name: "sort",
        description: "Sort lines of text files",
        flags: &[
            ("-u", "Output only unique lines (deduplicate)"),
            ("-r", "Reverse the result of comparisons"),
            ("-n", "Compare according to string numerical value"),
            ("-k <col>", "Sort by a specific key/column"),
        ],
        recipes: &[
            ("sort -u logs/client_ips.txt", "Sort and deduplicate client IP list"),
            ("sort -nr data/scores.txt", "Sort numerically in descending order"),
            ("cut -d',' -f2 data/customers.csv | sort", "Extract and sort customer names"),
        ],
    },
    CheatCard {
        name: "curl",
        description: "Transfer data from or to a server using HTTP/HTTPS",
        flags: &[
            ("-s", "Silent mode (don't show progress meter or error messages)"),
            ("-I", "Fetch HTTP headers only (HEAD request)"),
            ("-v", "Make operation verbose (show request/response headers)"),
            ("-X <METHOD>", "Specify custom HTTP request method (GET, POST, DELETE)"),
            ("-H <header>", "Pass custom HTTP header to server"),
            ("-d <data>", "HTTP POST data payload"),
        ],
        recipes: &[
            ("curl -I http://localhost:8080/health", "Inspect HTTP response status headers"),
            ("curl -s http://localhost:8080/api/customers | jq .", "Fetch customers API and format with jq"),
            ("curl -X POST -d '{\"status\":\"ready\"}' http://localhost:8080/api/worker", "Send JSON payload via POST"),
        ],
    },
    CheatCard {
        name: "service",
        description: "Manage simulated background daemons and service lifecycles",
        flags: &[
            ("status", "Show current running/crashed state, PID, and uptime"),
            ("start", "Start service and run configuration validation"),
            ("stop", "Cleanly shut down service"),
            ("restart", "Stop and restart service"),
            ("logs", "Inspect service output and crash tracebacks"),
        ],
        recipes: &[
            ("service status", "Display status overview of all mock services"),
            ("service start web", "Start the web payment HTTP service"),
            ("service logs web", "Inspect recent web service logs and errors"),
            ("service restart worker", "Restart the background queue worker"),
        ],
    },
    CheatCard {
        name: "xargs",
        description: "Build and execute command lines from standard input",
        flags: &[
            ("-I {}", "Replace occurrences of {} with input line"),
            ("-n <num>", "Use at most num arguments per command line"),
        ],
        recipes: &[
            ("find . -name '*.tmp' | xargs rm -f", "Delete all found temporary files"),
            ("cat hosts.txt | xargs -n 1 ping -c 1", "Ping each host in file one by one"),
        ],
    },
    CheatCard {
        name: "db",
        description: "Embedded SQLite client and relational query inspector",
        flags: &[
            ("schema", "Display table CREATE TABLE definitions"),
            ("tables", "List all active database tables"),
            ("<SQL>", "Execute SELECT or modifying SQL query"),
        ],
        recipes: &[
            ("db", "Display database tables, row counts, and summary"),
            ("db schema", "Inspect relational table structures and foreign keys"),
            ("db \"SELECT * FROM customers WHERE plan = 'enterprise'\"", "Filter customers by tier"),
            ("db \"SELECT c.name, o.amount FROM customers c JOIN orders o ON c.id = o.customer_id\"", "Perform relational table join"),
        ],
    },
    CheatCard {
        name: "ping",
        description: "Send ICMP echo requests to test host reachability and latency",
        flags: &[
            ("-c <num>", "Stop after sending <num> ECHO_REQUEST packets"),
        ],
        recipes: &[
            ("ping localhost", "Verify local network loopback interface"),
            ("ping -c 2 internal.api", "Test connectivity to internal microservice API"),
            ("ping 8.8.8.8 -c 1", "Probe external DNS reachability"),
        ],
    },
    CheatCard {
        name: "whatif",
        description: "Dry-run blast-radius analysis before running destructive commands",
        flags: &[
            ("<command>", "Any modifying command (rm, echo >, mv, truncate)"),
        ],
        recipes: &[
            ("whatif 'rm -rf logs/*.log'", "Preview targeted file deletions and freed disk bytes"),
            ("whatif 'rm -rf app/cache/'", "Verify what cache files would be removed"),
            ("whatif 'echo new > config/server.conf'", "Preview file truncation/overwrite"),
        ],
    },
    CheatCard {
        name: "undo",
        description: "Time-travel workspace recovery reverting previous modifying commands",
        flags: &[
            ("diff", "Show unified color diff of changes that will be reverted"),
        ],
        recipes: &[
            ("undo diff", "Preview differences between current workspace and pre-command snapshot"),
            ("undo", "Instantly restore workspace to state before previous modifying command"),
        ],
    },
    CheatCard {
        name: "tour",
        description: "Interactive flight simulator tour and feature exploration guide",
        flags: &[],
        recipes: &[
            ("tour", "Launch comprehensive tour of virtual lab and simulator tools"),
            ("cadet", "Inspect Cadet flight dossier and achievement badges"),
            ("drill", "List emergency chaos room incident scenarios"),
        ],
    },
];

pub fn run_cheat(tool: Option<&str>) -> String {
    let Some(name) = tool else {
        let mut out = String::new();
        out.push_str("\n\x1b[1;36m╔══════════════════════════════════════════════════════════════════════╗\x1b[0m\n");
        out.push_str("\x1b[1;36m║              📚   SHELLPILOT INSTANT UNIX CHEATSHEETS                ║\x1b[0m\n");
        out.push_str("\x1b[1;36m╚══════════════════════════════════════════════════════════════════════╝\x1b[0m\n\n");
        out.push_str("Available tool cheatsheets (type '\x1b[1;33mcheat <tool>\x1b[0m'):\n\n");

        for card in CHEAT_CARDS {
            out.push_str(&format!(
                "  • \x1b[1;32m{:<10}\x1b[0m : {}\n",
                card.name, card.description
            ));
        }
        out.push_str("\n\x1b[0;90m💡 Example: 'cheat grep', 'cheat chmod', 'cheat service'\x1b[0m\n\n");
        return out;
    };

    let target = name.trim().to_lowercase();
    let card = CHEAT_CARDS.iter().find(|c| c.name == target);

    match card {
        Some(c) => {
            let mut out = String::new();
            out.push_str(&format!("\n\x1b[1;36m📋 CHEATSHEET: {}\x1b[0m - {}\n\n", c.name, c.description));

            out.push_str("\x1b[1;35mKey Flags:\x1b[0m\n");
            for (flag, desc) in c.flags {
                out.push_str(&format!("  \x1b[1;33m{:<16}\x1b[0m {}\n", flag, desc));
            }

            out.push_str("\n\x1b[1;35mPractical Recipes:\x1b[0m\n");
            for (cmd, explanation) in c.recipes {
                out.push_str(&format!("  $ \x1b[1;32m{}\x1b[0m\n    \x1b[0;90m└─ {}\x1b[0m\n", cmd, explanation));
            }
            out.push('\n');
            out
        }
        None => {
            format!(
                "shellpilot: no cheatsheet for '{}'. Type 'cheat' to see available tools.\n",
                name
            )
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. THE TERMINAL DOCTOR (FAILURE DIAGNOSTIC)
// ─────────────────────────────────────────────────────────────────────────────

pub struct DiagnosticReport {
    pub title: String,
    pub explanation: String,
    pub suggested_command: Option<String>,
}

pub fn diagnose(last_command: &str, last_status: i32, current_dir: &Path) -> DiagnosticReport {
    if last_status == 0 {
        return DiagnosticReport {
            title: "Command Succeeded".to_string(),
            explanation: "The last command finished with exit status 0 (Success). Everything went as expected.".to_string(),
            suggested_command: None,
        };
    }

    let trimmed = last_command.trim();
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens.is_empty() {
        return DiagnosticReport {
            title: "Empty Command".to_string(),
            explanation: "No command was provided.".to_string(),
            suggested_command: None,
        };
    }

    let cmd = tokens[0];

    // Check 1: Nested directory creation without -p
    if cmd == "mkdir" && !tokens.contains(&"-p") {
        for arg in &tokens[1..] {
            if arg.contains('/') && !arg.starts_with('-') {
                return DiagnosticReport {
                    title: "Missing Parent Directory Flag (-p)".to_string(),
                    explanation: format!(
                        "'mkdir {}' failed because parent directories do not exist yet. Standard 'mkdir' requires parent directories to already be present.",
                        arg
                    ),
                    suggested_command: Some(format!("mkdir -p {}", tokens[1..].join(" "))),
                };
            }
        }
    }

    // Check 2: rm on directory without -r
    if cmd == "rm" && !tokens.contains(&"-r") && !tokens.contains(&"-rf") && !tokens.contains(&"-fr") {
        for arg in &tokens[1..] {
            let p = current_dir.join(arg);
            if p.is_dir() {
                return DiagnosticReport {
                    title: "Directory Removal Needs Recursive Flag (-r)".to_string(),
                    explanation: format!("'{}' is a directory. Plain 'rm' only removes files.", arg),
                    suggested_command: Some(format!("rm -r {}", arg)),
                };
            }
        }
    }

    // Check 3: Script execution without ./
    if cmd.ends_with(".sh") && !cmd.starts_with("./") && !cmd.starts_with('/') {
        if current_dir.join(cmd).exists() {
            return DiagnosticReport {
                title: "Relative Script Execution Needs './'".to_string(),
                explanation: "For security, Linux does not search the current directory for executables unless you specify './'.".to_string(),
                suggested_command: Some(format!("./{}", trimmed)),
            };
        }
        if current_dir.join("scripts").join(cmd).exists() {
            return DiagnosticReport {
                title: "Script Located in scripts/ Directory".to_string(),
                explanation: format!("'{}' was not found in the current folder, but exists in 'scripts/'.", cmd),
                suggested_command: Some(format!("./scripts/{}", trimmed)),
            };
        }
    }

    // Check 4: Typo in file arguments
    for arg in &tokens[1..] {
        if arg.starts_with('-') {
            continue;
        }
        let target_path = current_dir.join(arg);
        if !target_path.exists() {
            // Look for closest match in the same directory
            let parent = target_path.parent().unwrap_or(current_dir);
            let filename = target_path.file_name().and_then(|f| f.to_str()).unwrap_or("");

            if let Ok(entries) = fs::read_dir(parent) {
                let candidates: Vec<String> = entries
                    .flatten()
                    .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                    .collect();

                // Check prefix match or extension match
                for cand in candidates {
                    if cand.starts_with(filename) || cand.strip_suffix(".py") == Some(filename) || cand.strip_suffix(".conf") == Some(filename) {
                        let corrected = if let Some(parent_str) = target_path.parent().and_then(|p| p.strip_prefix(current_dir).ok()).and_then(|p| p.to_str()) {
                            if parent_str.is_empty() {
                                cand.clone()
                            } else {
                                format!("{}/{}", parent_str, cand)
                            }
                        } else {
                            cand.clone()
                        };

                        let mut new_tokens = tokens.clone();
                        if let Some(pos) = new_tokens.iter().position(|t| *t == *arg) {
                            new_tokens[pos] = &corrected;
                            return DiagnosticReport {
                                title: "File Not Found (Possible Typo)".to_string(),
                                explanation: format!("'{}' does not exist. Did you mean '{}'?", arg, corrected),
                                suggested_command: Some(new_tokens.join(" ")),
                            };
                        }
                    }
                }
            }
        }
    }

    // Check 5: Insecure chmod 777
    if cmd == "chmod" && tokens.contains(&"777") {
        return DiagnosticReport {
            title: "Security Warning: chmod 777".to_string(),
            explanation: "'chmod 777' grants full read, write, and execute permissions to all users on the system, creating a severe vulnerability.".to_string(),
            suggested_command: Some("chmod 755 <path>  (or 'chmod 644 <path>' for non-executables)".to_string()),
        };
    }

    DiagnosticReport {
        title: format!("Command Failed (Exit Code {})", last_status),
        explanation: format!(
            "Command '{}' returned status {}. Check file paths, flags, or syntax.",
            trimmed, last_status
        ),
        suggested_command: None,
    }
}

pub fn suggest_inline_hint(last_command: &str, last_status: i32, current_dir: &Path) -> Option<String> {
    if last_status == 0 {
        return None;
    }
    let diag = diagnose(last_command, last_status, current_dir);
    if let Some(cmd) = diag.suggest_command() {
        Some(format!("💡 \x1b[1;33mDoctor:\x1b[0m {} Try: '\x1b[1;32m{}\x1b[0m'", diag.explanation, cmd))
    } else if diag.title != format!("Command Failed (Exit Code {})", last_status) {
        Some(format!("💡 \x1b[1;33mDoctor:\x1b[0m {}", diag.explanation))
    } else {
        None
    }
}

impl DiagnosticReport {
    pub fn suggest_command(&self) -> Option<&str> {
        self.suggested_command.as_deref()
    }

    pub fn format(&self) -> String {
        let mut out = String::new();
        out.push_str("\n\x1b[1;36m╔══════════════════════════════════════════════════════════════════════╗\x1b[0m\n");
        out.push_str(&format!("\x1b[1;36m║  🩺 THE TERMINAL DOCTOR: {:<42} ║\x1b[0m\n", self.title));
        out.push_str("\x1b[1;36m╚══════════════════════════════════════════════════════════════════════╝\x1b[0m\n\n");
        out.push_str(&format!("📖 \x1b[1mDiagnosis:\x1b[0m\n   {}\n\n", self.explanation));

        if let Some(ref fix) = self.suggested_command {
            out.push_str(&format!("💡 \x1b[1mSuggested Correction:\x1b[0m\n   $ \x1b[1;32m{}\x1b[0m\n\n", fix));
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_renders_hierarchy_and_depth() {
        let path = std::env::temp_dir().join(format!("shellpilot_test_tree_{}", std::process::id()));
        fs::create_dir_all(path.join("a/b")).unwrap();
        fs::write(path.join("a/b/file.txt"), "hello").unwrap();
        fs::write(path.join("a/script.sh"), "#!/bin/bash").unwrap();

        let opts = TreeOptions {
            max_depth: 3,
            dirs_only: false,
        };
        let tree = render_tree(&path, &opts).unwrap();
        assert!(tree.contains("a"));
        assert!(tree.contains("script.sh"));
        assert!(tree.contains("2 directories, 2 files"));

        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn cheat_returns_correct_recipes() {
        let grep_cheat = run_cheat(Some("grep"));
        assert!(grep_cheat.contains("CHEATSHEET: grep"));
        assert!(grep_cheat.contains("-i"));
        assert!(grep_cheat.contains("Case-insensitive search"));

        let list = run_cheat(None);
        assert!(list.contains("SHELLPILOT INSTANT UNIX CHEATSHEETS"));
        assert!(list.contains("grep"));
        assert!(list.contains("chmod"));
    }

    #[test]
    fn doctor_diagnoses_mkdir_parent_error() {
        let path = std::env::temp_dir().join(format!("shellpilot_test_doc1_{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        let diag = diagnose("mkdir app/sub/v2", 1, &path);
        assert_eq!(diag.title, "Missing Parent Directory Flag (-p)");
        assert_eq!(diag.suggest_command(), Some("mkdir -p app/sub/v2"));
        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn doctor_diagnoses_rm_directory_error() {
        let path = std::env::temp_dir().join(format!("shellpilot_test_doc2_{}", std::process::id()));
        let sub = path.join("logs");
        fs::create_dir_all(&sub).unwrap();

        let diag = diagnose("rm logs", 1, &path);
        assert_eq!(diag.title, "Directory Removal Needs Recursive Flag (-r)");
        assert_eq!(diag.suggest_command(), Some("rm -r logs"));
        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn doctor_diagnoses_file_typo() {
        let path = std::env::temp_dir().join(format!("shellpilot_test_doc3_{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("server.py"), "print()").unwrap();

        let diag = diagnose("cat server", 1, &path);
        assert_eq!(diag.title, "File Not Found (Possible Typo)");
        assert_eq!(diag.suggest_command(), Some("cat server.py"));
        fs::remove_dir_all(&path).ok();
    }
}
