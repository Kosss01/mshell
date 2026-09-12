use std::collections::HashSet;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationResult {
    Success { feedback: String },
    Incomplete { reason: String },
    Failed { error: String },
}

pub struct Lesson {
    pub id: &'static str,
    #[allow(dead_code)]
    pub track_id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub instructions: &'static str,
    pub hints: &'static [&'static str],
    pub solution: &'static str,
    pub setup: fn(workspace: &Path) -> io::Result<()>,
    pub validate: fn(workspace: &Path, last_command: Option<&str>) -> ValidationResult,
}

pub struct LessonTrack {
    #[allow(dead_code)]
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub lessons: Vec<Lesson>,
}

pub struct TutorEngine {
    pub tracks: Vec<LessonTrack>,
    pub active_lesson_id: Option<String>,
    pub hint_level: usize,
    pub completed_lessons: HashSet<String>,
}

fn find_lesson_in_tracks<'a>(tracks: &'a [LessonTrack], id: &str) -> Option<&'a Lesson> {
    for track in tracks {
        for lesson in &track.lessons {
            if lesson.id == id {
                return Some(lesson);
            }
        }
    }
    None
}

impl TutorEngine {
    pub fn new(db: Option<&rusqlite::Connection>) -> Self {
        let mut completed = HashSet::new();
        if let Some(conn) = db {
            conn.execute(
                "CREATE TABLE IF NOT EXISTS tutor_progress (
                    lesson_id TEXT PRIMARY KEY,
                    completed_at INTEGER
                )",
                [],
            )
            .ok();

            if let Ok(mut stmt) = conn.prepare("SELECT lesson_id FROM tutor_progress")
                && let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
                    for r in rows.flatten() {
                        completed.insert(r);
                    }
                }
        }

        Self {
            tracks: build_all_tracks(),
            active_lesson_id: None,
            hint_level: 0,
            completed_lessons: completed,
        }
    }

    #[allow(dead_code)]
    pub fn find_lesson(&self, id: &str) -> Option<&Lesson> {
        find_lesson_in_tracks(&self.tracks, id)
    }

    pub fn first_incomplete_lesson(&self) -> Option<&Lesson> {
        let all_lessons: Vec<&Lesson> = self.tracks.iter().flat_map(|t| &t.lessons).collect();
        all_lessons
            .into_iter()
            .find(|l| !self.completed_lessons.contains(l.id))
    }

    pub fn list_tracks_overview(&self) -> String {
        let mut out = String::new();
        out.push_str("\n╔══════════════════════════════════════════════════════════════════════════╗\n");
        out.push_str("║              🎓  SHELLPILOT INTERACTIVE FLIGHT ACADEMY                   ║\n");
        out.push_str("╚══════════════════════════════════════════════════════════════════════════╝\n\n");

        for track in &self.tracks {
            let total = track.lessons.len();
            let done = track
                .lessons
                .iter()
                .filter(|l| self.completed_lessons.contains(l.id))
                .count();

            let percent = (done * 10).checked_div(total).unwrap_or(0);
            let bar = format!("{}{}", "█".repeat(percent), "░".repeat(10 - percent));

            out.push_str(&format!(
                "📂 TRACK: {} [{}] ({}/{})\n   {}\n",
                track.title, bar, done, total, track.description
            ));

            for lesson in &track.lessons {
                let status = if self.completed_lessons.contains(lesson.id) {
                    "✓ [DONE]"
                } else if self.active_lesson_id.as_deref() == Some(lesson.id) {
                    "▶ [ACTIVE]"
                } else {
                    "○ [TODO]"
                };

                out.push_str(&format!("     {status:<10} {:<10} - {}\n", lesson.id, lesson.title));
            }
            out.push('\n');
        }

        out.push_str("💡 Commands:\n");
        out.push_str("   tutor start <lesson_id>   Begin a specific lesson (e.g. 'tutor start nav_01')\n");
        out.push_str("   tutor check               Evaluate whether current challenge is complete\n");
        out.push_str("   tutor hint                Get a helpful hint if stuck\n");
        out.push_str("   tutor solution            Reveal the reference solution and explanation\n");
        out.push_str("   tutor reset               Reset workspace to initial challenge state\n");
        out.push_str("   tutor next                Advance to the next challenge\n");

        out
    }

    pub fn start_lesson(&mut self, lesson_id: &str, workspace: &Path) -> io::Result<String> {
        let lesson = match find_lesson_in_tracks(&self.tracks, lesson_id) {
            Some(l) => l,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Unknown lesson ID '{lesson_id}'. Run 'tutor' to see available IDs."),
                ));
            }
        };

        // Clean and prepare workspace with the base virtual server ecosystem
        if workspace.exists() {
            if let Ok(entries) = fs::read_dir(workspace) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let _ = fs::remove_dir_all(path);
                    } else {
                        let _ = fs::remove_file(path);
                    }
                }
            }
        } else {
            fs::create_dir_all(workspace)?;
        }
        crate::sandbox::populate_sandbox_ecosystem(workspace)?;

        // Execute lesson setup
        (lesson.setup)(workspace)?;

        let id_str = lesson.id;
        let title_str = lesson.title;
        let desc_str = lesson.description;
        let instr_str = lesson.instructions;

        self.active_lesson_id = Some(lesson_id.to_string());
        self.hint_level = 0;

        let mut out = String::new();
        out.push_str(&format!(
            "\n╔══════════════════════════════════════════════════════════════════════╗\n\
             ║  🎓 Lesson {}: {} \n\
             ╚══════════════════════════════════════════════════════════════════════╝\n\n",
            id_str, title_str
        ));
        out.push_str(&format!("📖 Background:\n  {}\n\n", desc_str));
        out.push_str(&format!("🎯 Objective:\n{}\n\n", instr_str));
        out.push_str("💡 Tip: When you think you're done, run 'tutor check' or execute your command.\n");

        Ok(out)
    }

    pub fn evaluate_current(&self, workspace: &Path, last_cmd: Option<&str>) -> Option<ValidationResult> {
        let lesson_id = self.active_lesson_id.as_ref()?;
        let lesson = find_lesson_in_tracks(&self.tracks, lesson_id)?;
        Some((lesson.validate)(workspace, last_cmd))
    }

    pub fn next_hint(&mut self) -> Option<String> {
        let lesson_id = self.active_lesson_id.clone()?;
        let lesson = find_lesson_in_tracks(&self.tracks, &lesson_id)?;

        if lesson.hints.is_empty() {
            return Some("No hints available for this challenge.".to_string());
        }

        if self.hint_level < lesson.hints.len() {
            let hint = lesson.hints[self.hint_level];
            let count = lesson.hints.len();
            self.hint_level += 1;
            Some(format!(
                "💡 Hint ({}/{}):\n   {}",
                self.hint_level,
                count,
                hint
            ))
        } else {
            Some(format!(
                "💡 You have seen all hints! Reference solution is:\n   {}",
                lesson.solution
            ))
        }
    }

    pub fn get_solution(&self) -> Option<String> {
        let lesson_id = self.active_lesson_id.as_ref()?;
        let lesson = find_lesson_in_tracks(&self.tracks, lesson_id)?;
        Some(format!(
            "🔑 Reference Solution for {}:\n   {}\n",
            lesson.id, lesson.solution
        ))
    }

    pub fn reset_lesson(&mut self, workspace: &Path) -> io::Result<String> {
        let lesson_id = match self.active_lesson_id.clone() {
            Some(id) => id,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "No active lesson to reset. Start one with 'tutor start <id>'.",
                ));
            }
        };

        self.start_lesson(&lesson_id, workspace)
    }

    pub fn advance_to_next(&mut self, workspace: &Path) -> io::Result<String> {
        let current_id = match self.active_lesson_id.as_ref() {
            Some(id) => id.clone(),
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "No active lesson. Choose one from 'tutor list'.",
                ));
            }
        };

        // Flatten all lessons across all tracks to find the subsequent one
        let all_lessons: Vec<&Lesson> = self.tracks.iter().flat_map(|t| &t.lessons).collect();
        let current_index = all_lessons.iter().position(|l| l.id == current_id);

        match current_index {
            Some(idx) if idx + 1 < all_lessons.len() => {
                let next_id = all_lessons[idx + 1].id;
                self.start_lesson(next_id, workspace)
            }
            _ => Ok("🏆 Congratulations! You have completed all lessons in the Academy!".to_string()),
        }
    }

    pub fn mark_completed(&mut self, lesson_id: &str, db: Option<&rusqlite::Connection>) {
        self.completed_lessons.insert(lesson_id.to_string());
        if let Some(conn) = db {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;

            conn.execute(
                "INSERT OR REPLACE INTO tutor_progress (lesson_id, completed_at) VALUES (?1, ?2)",
                rusqlite::params![lesson_id, timestamp],
            )
            .ok();
        }
    }
}

fn build_all_tracks() -> Vec<LessonTrack> {
    vec![
        build_cadet_track(),
        build_plumber_track(),
        build_guardian_track(),
        build_detective_track(),
        build_incident_track(),
        build_sre_track(),
        build_data_alchemist_track(),
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// TRACK 1: CADET (Filesystem Navigation)
// ─────────────────────────────────────────────────────────────────────────────
fn build_cadet_track() -> LessonTrack {
    LessonTrack {
        id: "cadet",
        title: "Cadet (Filesystem & Navigation)",
        description: "Master absolute vs relative paths, directory structures, and wildcards.",
        lessons: vec![
            Lesson {
                id: "nav_01",
                track_id: "cadet",
                title: "Server Orientation & Readme",
                description: "Inspect the staging server structure and read the orientation guide.",
                instructions: "Inspect the current folder with 'ls'. Read 'README.md' to orient yourself, then copy 'README.md' to 'docs/overview.txt' using:\nmkdir -p docs && cp README.md docs/overview.txt",
                hints: &[
                    "List directory contents with 'ls -la' to see app/, config/, logs/, data/, scripts/.",
                    "Read the readme with 'cat README.md'.",
                    "Create docs and copy: 'mkdir -p docs && cp README.md docs/overview.txt'.",
                ],
                solution: "mkdir -p docs && cp README.md docs/overview.txt",
                setup: |_ws| Ok(()),
                validate: |ws, _last_cmd| {
                    let target = ws.join("docs").join("overview.txt");
                    if target.exists()
                        && let Ok(content) = fs::read_to_string(&target)
                            && content.contains("Simulated Server Environment") {
                                return ValidationResult::Success {
                                    feedback: "Excellent! You explored the server and created docs/overview.txt.".into(),
                                };
                            }
                    ValidationResult::Incomplete {
                        reason: "Create 'docs/' and copy 'README.md' to 'docs/overview.txt'.".into(),
                    }
                },
            },
            Lesson {
                id: "nav_02",
                track_id: "cadet",
                title: "Nested Directory Surgery",
                description: "Creating deep directory trees with mkdir -p.",
                instructions: "The payment team needs directories for a new v2 microservice. Create 'services/payment/handlers/v2' and 'services/payment/tests' in a single command using 'mkdir -p'.",
                hints: &[
                    "Without '-p', mkdir fails if parent folders do not exist.",
                    "Pass multiple target paths: 'mkdir -p services/payment/handlers/v2 services/payment/tests'.",
                ],
                solution: "mkdir -p services/payment/handlers/v2 services/payment/tests",
                setup: |_ws| Ok(()),
                validate: |ws, _last_cmd| {
                    let d1 = ws.join("services").join("payment").join("handlers").join("v2");
                    let d2 = ws.join("services").join("payment").join("tests");
                    if d1.is_dir() && d2.is_dir() {
                        ValidationResult::Success {
                            feedback: "Awesome! Both nested service directories were successfully created.".into(),
                        }
                    } else {
                        ValidationResult::Incomplete {
                            reason: "Directories 'services/payment/handlers/v2' and 'services/payment/tests' must both exist.".into(),
                        }
                    }
                },
            },
            Lesson {
                id: "nav_03",
                track_id: "cadet",
                title: "Staging and Archiving Configurations",
                description: "Safely backing up and renaming production settings.",
                instructions: "1. Create directory 'backups/'.\n2. Copy 'config/server.conf' to 'backups/server.conf.bak'.\n3. Rename/move 'config/settings.env' to 'config/.env.production'.",
                hints: &[
                    "Use 'mkdir -p backups' to create the archive folder.",
                    "Use 'cp config/server.conf backups/server.conf.bak' to duplicate the file.",
                    "Use 'mv config/settings.env config/.env.production' to rename the env file.",
                ],
                solution: "mkdir -p backups && cp config/server.conf backups/server.conf.bak && mv config/settings.env config/.env.production",
                setup: |_ws| Ok(()),
                validate: |ws, _last_cmd| {
                    let backup = ws.join("backups").join("server.conf.bak");
                    let new_env = ws.join("config").join(".env.production");
                    let old_env = ws.join("config").join("settings.env");

                    if backup.exists() && new_env.exists() && !old_env.exists() {
                        ValidationResult::Success {
                            feedback: "Spot on! Configuration backup created and settings file moved to .env.production.".into(),
                        }
                    } else if !backup.exists() {
                        ValidationResult::Incomplete {
                            reason: "'backups/server.conf.bak' is missing.".into(),
                        }
                    } else {
                        ValidationResult::Incomplete {
                            reason: "'config/.env.production' is missing or 'config/settings.env' was not moved.".into(),
                        }
                    }
                },
            },
            Lesson {
                id: "nav_04",
                track_id: "cadet",
                title: "Wildcards and Cache Purging",
                description: "Using glob patterns to remove temporary files without affecting critical files.",
                instructions: "Delete all '*.tmp' and '*.cache' files inside 'app/cache/' using wildcards (e.g. 'rm app/cache/*.tmp app/cache/*.cache'), while leaving 'cache_manifest.json' and '.gitkeep' intact!",
                hints: &[
                    "The '*' wildcard matches any sequence of characters.",
                    "Run 'rm app/cache/*.tmp app/cache/*.cache'.",
                    "Do NOT delete 'cache_manifest.json' or '.gitkeep'!",
                ],
                solution: "rm app/cache/*.tmp app/cache/*.cache",
                setup: |ws| {
                    let cache = ws.join("app").join("cache");
                    fs::create_dir_all(&cache)?;
                    fs::write(cache.join("sess_9011.tmp"), "session1")?;
                    fs::write(cache.join("sess_9012.tmp"), "session2")?;
                    fs::write(cache.join("query_users.cache"), "cache1")?;
                    fs::write(cache.join("query_items.cache"), "cache2")?;
                    fs::write(cache.join("cache_manifest.json"), "{\"version\": 1}\n")?;
                    fs::write(cache.join(".gitkeep"), "")?;
                    Ok(())
                },
                validate: |ws, _last_cmd| {
                    let cache = ws.join("app").join("cache");
                    let manifest_exists = cache.join("cache_manifest.json").exists();
                    let gitkeep_exists = cache.join(".gitkeep").exists();

                    if !manifest_exists || !gitkeep_exists {
                        return ValidationResult::Failed {
                            error: "Oops! 'cache_manifest.json' or '.gitkeep' was deleted. Run 'tutor reset' to try again.".into(),
                        };
                    }

                    let has_tmp_or_cache = fs::read_dir(&cache)
                        .ok()
                        .map(|entries| {
                            entries.flatten().any(|e| {
                                let name = e.file_name().to_string_lossy().to_string();
                                name.ends_with(".tmp") || name.ends_with(".cache")
                            })
                        })
                        .unwrap_or(false);

                    if !has_tmp_or_cache {
                        ValidationResult::Success {
                            feedback: "Clean sweep! All temporary cache and session files removed while preserving critical manifests.".into(),
                        }
                    } else {
                        ValidationResult::Incomplete {
                            reason: "There are still .tmp or .cache files remaining in 'app/cache/'.".into(),
                        }
                    }
                },
            },
        ],
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TRACK 2: THE PLUMBER (Streams & Redirections)
// ─────────────────────────────────────────────────────────────────────────────
fn build_plumber_track() -> LessonTrack {
    LessonTrack {
        id: "plumber",
        title: "The Plumber (Streams & Redirection)",
        description: "Master standard I/O streams, overwriting, appending, merging, and pipes.",
        lessons: vec![
            Lesson {
                id: "pipe_01",
                track_id: "plumber",
                title: "Output Redirection: > vs >>",
                description: "Writing stdout to files: overwrite vs append.",
                instructions: "1. Write 'SYSTEM AUDIT INITIALIZED' to 'logs/audit.log' using '>'.\n2. Append 'AUDIT RUNNER: root' on a new line using '>>'.",
                hints: &[
                    "echo 'SYSTEM AUDIT INITIALIZED' > logs/audit.log",
                    "echo 'AUDIT RUNNER: root' >> logs/audit.log",
                ],
                solution: "echo 'SYSTEM AUDIT INITIALIZED' > logs/audit.log && echo 'AUDIT RUNNER: root' >> logs/audit.log",
                setup: |ws| {
                    let audit = ws.join("logs").join("audit.log");
                    if audit.exists() {
                        fs::remove_file(audit).ok();
                    }
                    Ok(())
                },
                validate: |ws, _last_cmd| {
                    let path = ws.join("logs").join("audit.log");
                    if let Ok(content) = fs::read_to_string(&path) {
                        let lines: Vec<&str> = content.lines().collect();
                        if lines.len() >= 2
                            && lines[0].contains("SYSTEM AUDIT INITIALIZED")
                            && lines[1].contains("AUDIT RUNNER: root")
                        {
                            return ValidationResult::Success {
                                feedback: "Great! You mastered overwriting (>) and appending (>>).".into(),
                            };
                        }
                    }
                    ValidationResult::Incomplete {
                        reason: "'logs/audit.log' must have 'SYSTEM AUDIT INITIALIZED' on line 1 and 'AUDIT RUNNER: root' on line 2.".into(),
                    }
                },
            },
            Lesson {
                id: "pipe_02",
                track_id: "plumber",
                title: "Redirecting Standard Error (2>)",
                description: "Separating diagnostics and error logs with file descriptor 2.",
                instructions: "Run './scripts/healthcheck.sh' redirecting its standard error (stderr) to 'logs/health_warnings.log' using '2>'.",
                hints: &[
                    "Standard error uses file descriptor 2.",
                    "Run: ./scripts/healthcheck.sh 2> logs/health_warnings.log",
                ],
                solution: "./scripts/healthcheck.sh 2> logs/health_warnings.log",
                setup: |_ws| Ok(()),
                validate: |ws, _last_cmd| {
                    let err_file = ws.join("logs").join("health_warnings.log");
                    if let Ok(content) = fs::read_to_string(&err_file)
                        && content.contains("WARNING: Memory swap usage at 78%") {
                            return ValidationResult::Success {
                                feedback: "Excellent! Standard error was cleanly separated into logs/health_warnings.log.".into(),
                            };
                        }
                    ValidationResult::Incomplete {
                        reason: "'logs/health_warnings.log' does not yet contain the captured error output.".into(),
                    }
                },
            },
            Lesson {
                id: "pipe_03",
                track_id: "plumber",
                title: "Pipeline Data Flow (|)",
                description: "Chaining standard output into another process's standard input.",
                instructions: "Count how many times HTTP 404 status codes appear in 'logs/access.log' using grep and wc, saving count to 'logs/404_count.txt'.\nE.g.: grep ' 404 ' logs/access.log | wc -l > logs/404_count.txt",
                hints: &[
                    "Combine grep and wc using pipe '|' and redirect to 'logs/404_count.txt' with '>'.",
                    "grep ' 404 ' logs/access.log | wc -l > logs/404_count.txt",
                ],
                solution: "grep ' 404 ' logs/access.log | wc -l > logs/404_count.txt",
                setup: |_ws| Ok(()),
                validate: |ws, _last_cmd| {
                    let out = ws.join("logs").join("404_count.txt");
                    if let Ok(content) = fs::read_to_string(&out)
                        && content.trim() == "3" {
                            return ValidationResult::Success {
                                feedback: "Awesome! You chained grep, wc, and redirection into logs/404_count.txt.".into(),
                            };
                        }
                    ValidationResult::Incomplete {
                        reason: "'logs/404_count.txt' should contain the count '3'.".into(),
                    }
                },
            },
            Lesson {
                id: "pipe_04",
                track_id: "plumber",
                title: "Stream Filtering with cut and sort",
                description: "Transforming log fields and deduplicating streams.",
                instructions: "Extract the client IP addresses (column 1) from 'logs/access.log', sort them, remove duplicates, and write the output to 'logs/client_ips.txt'.\nE.g.: cut -d' ' -f1 logs/access.log | sort -u > logs/client_ips.txt",
                hints: &[
                    "Use cut with space delimiter: 'cut -d\" \" -f1 logs/access.log'.",
                    "Pipe into sort with unique flag: '| sort -u'.",
                    "Redirect output: '> logs/client_ips.txt'.",
                ],
                solution: "cut -d' ' -f1 logs/access.log | sort -u > logs/client_ips.txt",
                setup: |_ws| Ok(()),
                validate: |ws, _last_cmd| {
                    let out = ws.join("logs").join("client_ips.txt");
                    if let Ok(content) = fs::read_to_string(&out) {
                        let lines: Vec<&str> = content.lines().collect();
                        if lines == vec![
                            "10.0.0.55",
                            "10.0.0.99",
                            "172.16.0.12",
                            "172.16.0.4",
                            "192.168.1.100",
                            "192.168.1.101",
                            "192.168.1.105",
                        ] {
                            return ValidationResult::Success {
                                feedback: "Brilliant! You extracted and deduplicated the client IP addresses.".into(),
                            };
                        }
                    }
                    ValidationResult::Incomplete {
                        reason: "'logs/client_ips.txt' must contain sorted, unique IP addresses from 'logs/access.log'.".into(),
                    }
                },
            },
        ],
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TRACK 3: THE GUARDIAN (Permissions & Security)
// ─────────────────────────────────────────────────────────────────────────────
fn build_guardian_track() -> LessonTrack {
    LessonTrack {
        id: "guardian",
        title: "The Guardian (Permissions & Security)",
        description: "Understand read, write, and execute bits, symbolic vs octal chmod, and safe modes.",
        lessons: vec![
            Lesson {
                id: "perm_01",
                track_id: "guardian",
                title: "Making Deployment Scripts Executable",
                description: "Granting execution permissions with chmod +x or chmod 755.",
                instructions: "'scripts/deploy.sh' is currently not executable (mode 644). Grant execution permission to it using 'chmod +x scripts/deploy.sh'.",
                hints: &[
                    "Use 'chmod +x scripts/deploy.sh' or 'chmod 755 scripts/deploy.sh'.",
                ],
                solution: "chmod +x scripts/deploy.sh",
                setup: |ws| {
                    let file = ws.join("scripts").join("deploy.sh");
                    fs::write(&file, "#!/bin/sh\necho 'Deploying application...'\n")?;
                    #[cfg(unix)]
                    {
                        let mut perms = fs::metadata(&file)?.permissions();
                        perms.set_mode(0o644);
                        fs::set_permissions(&file, perms)?;
                    }
                    Ok(())
                },
                validate: |ws, _last_cmd| {
                    #[cfg(unix)]
                    {
                        let file = ws.join("scripts").join("deploy.sh");
                        if let Ok(meta) = fs::metadata(&file) {
                            let mode = meta.permissions().mode();
                            if mode & 0o111 != 0 {
                                return ValidationResult::Success {
                                    feedback: "Well done! 'scripts/deploy.sh' is now executable.".into(),
                                };
                            }
                        }
                    }
                    ValidationResult::Incomplete {
                        reason: "'scripts/deploy.sh' does not have execute permissions.".into(),
                    }
                },
            },
            Lesson {
                id: "perm_02",
                track_id: "guardian",
                title: "Securing Sensitive Private Keys",
                description: "Protecting secret keys by restricting permissions to owner-only.",
                instructions: "'keys/deploy_key.pem' is currently readable by everyone (mode 666). Restrict it so ONLY the owner can read/write (mode 600).",
                hints: &[
                    "600 means: User read/write (4+2), Group none (0), Others none (0).",
                    "Run 'chmod 600 keys/deploy_key.pem'.",
                ],
                solution: "chmod 600 keys/deploy_key.pem",
                setup: |ws| {
                    let file = ws.join("keys").join("deploy_key.pem");
                    #[cfg(unix)]
                    {
                        if file.exists() {
                            let mut perms = fs::metadata(&file)?.permissions();
                            perms.set_mode(0o666);
                            fs::set_permissions(&file, perms)?;
                        }
                    }
                    Ok(())
                },
                validate: |ws, _last_cmd| {
                    #[cfg(unix)]
                    {
                        let file = ws.join("keys").join("deploy_key.pem");
                        if let Ok(meta) = fs::metadata(&file) {
                            let mode = meta.permissions().mode() & 0o777;
                            if mode == 0o600 {
                                return ValidationResult::Success {
                                    feedback: "Security locked down! 'keys/deploy_key.pem' is now mode 600 (owner only).".into(),
                                };
                            }
                        }
                    }
                    ValidationResult::Incomplete {
                        reason: "'keys/deploy_key.pem' should have octal mode 600.".into(),
                    }
                },
            },
            Lesson {
                id: "perm_03",
                track_id: "guardian",
                title: "Read-Only Configuration Hardening",
                description: "Locking down critical configuration files against accidental modification.",
                instructions: "Harden 'config/database.yaml' by stripping all write permissions for everyone (set to read-only mode 444 or 'chmod a-w config/database.yaml').",
                hints: &[
                    "Mode 444 means read-only for user, group, and others (r--r--r--).",
                    "Run 'chmod 444 config/database.yaml' or 'chmod a-w config/database.yaml'.",
                ],
                solution: "chmod 444 config/database.yaml",
                setup: |ws| {
                    let file = ws.join("config").join("database.yaml");
                    #[cfg(unix)]
                    {
                        if file.exists() {
                            let mut perms = fs::metadata(&file)?.permissions();
                            perms.set_mode(0o644);
                            fs::set_permissions(&file, perms)?;
                        }
                    }
                    Ok(())
                },
                validate: |ws, _last_cmd| {
                    #[cfg(unix)]
                    {
                        let file = ws.join("config").join("database.yaml");
                        if let Ok(meta) = fs::metadata(&file) {
                            let mode = meta.permissions().mode() & 0o777;
                            if mode & 0o222 == 0 {
                                return ValidationResult::Success {
                                    feedback: "Hardening verified! Write permissions completely stripped from config/database.yaml.".into(),
                                };
                            }
                        }
                    }
                    ValidationResult::Incomplete {
                        reason: "'config/database.yaml' still has write permissions enabled.".into(),
                    }
                },
            },
        ],
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TRACK 4: THE DETECTIVE (Text Processing & Searching)
// ─────────────────────────────────────────────────────────────────────────────
fn build_detective_track() -> LessonTrack {
    LessonTrack {
        id: "detective",
        title: "The Detective (Searching & Text Processing)",
        description: "Master grep, sort, uniq, and searching tools to inspect system data.",
        lessons: vec![
            Lesson {
                id: "find_01",
                track_id: "detective",
                title: "Case-Insensitive Searching with Grep",
                description: "Extracting security authentication failures with grep -i.",
                instructions: "Search for the word 'failed' (ignoring case) in 'logs/auth.log' using 'grep -i' and write the matching incident lines to 'logs/failed_logins.txt'.",
                hints: &[
                    "Use the '-i' flag for case-insensitive search.",
                    "grep -i 'failed' logs/auth.log > logs/failed_logins.txt",
                ],
                solution: "grep -i 'failed' logs/auth.log > logs/failed_logins.txt",
                setup: |_ws| Ok(()),
                validate: |ws, _last_cmd| {
                    let out = ws.join("logs").join("failed_logins.txt");
                    if let Ok(content) = fs::read_to_string(&out) {
                        let lines: Vec<&str> = content.lines().collect();
                        if lines.len() == 3
                            && content.contains("invalid user root")
                            && content.contains("invalid user admin")
                            && content.contains("user bob")
                        {
                            return ValidationResult::Success {
                                feedback: "Great deduction! Case-insensitive matching caught all 3 security incidents.".into(),
                            };
                        }
                    }
                    ValidationResult::Incomplete {
                        reason: "'logs/failed_logins.txt' must contain all 3 matching failure lines from 'logs/auth.log'.".into(),
                    }
                },
            },
            Lesson {
                id: "find_02",
                track_id: "detective",
                title: "Locating Files with 'find'",
                description: "Finding scattered ad-hoc backup files across directory hierarchies.",
                instructions: "Find all files ending in '.bak' across the repository and save their relative paths to 'backups/stale_backups.txt'.\nE.g.: mkdir -p backups && find . -name \"*.bak\" | sort > backups/stale_backups.txt",
                hints: &[
                    "Use 'find . -name \"*.bak\"' to search recursively from current directory.",
                    "Run: mkdir -p backups && find . -name \"*.bak\" | sort > backups/stale_backups.txt",
                ],
                solution: "mkdir -p backups && find . -name \"*.bak\" | sort > backups/stale_backups.txt",
                setup: |ws| {
                    fs::write(ws.join("app").join("server.py.bak"), "old code")?;
                    fs::write(ws.join("config").join("server.conf.bak"), "old conf")?;
                    fs::write(ws.join("data").join("customers.csv.bak"), "old data")?;
                    Ok(())
                },
                validate: |ws, _last_cmd| {
                    let out = ws.join("backups").join("stale_backups.txt");
                    if let Ok(content) = fs::read_to_string(&out)
                        && content.contains("server.py.bak")
                            && content.contains("server.conf.bak")
                            && content.contains("customers.csv.bak")
                        {
                            return ValidationResult::Success {
                                feedback: "Sharp eye! All scattered backup files located and indexed.".into(),
                            };
                        }
                    ValidationResult::Incomplete {
                        reason: "'backups/stale_backups.txt' must contain the paths to the 3 .bak files.".into(),
                    }
                },
            },
            Lesson {
                id: "find_03",
                track_id: "detective",
                title: "Extracting and Sorting Structured CSV Data",
                description: "Processing tabular customer data without heavy tools.",
                instructions: "Extract the customer email column (column 3) from 'data/customers.csv', skip the header row, sort the emails alphabetically, and write them to 'data/customer_emails.txt'.\nE.g.: tail -n +2 data/customers.csv | cut -d',' -f3 | sort > data/customer_emails.txt",
                hints: &[
                    "Use 'tail -n +2 data/customers.csv' to skip the header line.",
                    "Use 'cut -d\",\" -f3' to select the email column.",
                    "Pipe into 'sort' and redirect to 'data/customer_emails.txt'.",
                ],
                solution: "tail -n +2 data/customers.csv | cut -d',' -f3 | sort > data/customer_emails.txt",
                setup: |_ws| Ok(()),
                validate: |ws, _last_cmd| {
                    let out = ws.join("data").join("customer_emails.txt");
                    if let Ok(content) = fs::read_to_string(&out) {
                        let lines: Vec<&str> = content.lines().collect();
                        if lines == vec![
                            "alice@acme.com",
                            "bob@techcorp.io",
                            "charlie@startup.dev",
                            "diana@themyscira.gov",
                            "evan@devops.co",
                            "fiona@shamrock.org",
                            "george@cloudscale.net",
                            "hannah@potion.co.uk",
                        ] {
                            return ValidationResult::Success {
                                feedback: "Outstanding analysis! Customer email directory extracted and sorted.".into(),
                            };
                        }
                    }
                    ValidationResult::Incomplete {
                        reason: "'data/customer_emails.txt' must contain the 8 sorted customer email addresses.".into(),
                    }
                },
            },
        ],
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TRACK 5: INCIDENT RESPONDER (Troubleshooting Scenarios)
// ─────────────────────────────────────────────────────────────────────────────
fn build_incident_track() -> LessonTrack {
    LessonTrack {
        id: "incident",
        title: "Incident Responder (Troubleshooting Scenarios)",
        description: "Solve real-world DevOps incidents: full disks, corrupted configs, and lockfiles.",
        lessons: vec![
            Lesson {
                id: "inc_01",
                track_id: "incident",
                title: "Incident: Runaway Disk Space",
                description: "A runaway debug trace is consuming disk. Safely truncate it without deleting the file.",
                instructions: "Alert: Disk space at 99%! A runaway trace log 'logs/debug_huge.log' is filling up the disk. Truncate it to 0 bytes (e.g. ': > logs/debug_huge.log' or 'truncate -s 0 logs/debug_huge.log'), while keeping 'logs/access.log' untouched!",
                hints: &[
                    "Inspect 'logs/' with 'ls -lh logs/'.",
                    "Truncate a file to 0 bytes with ': > logs/debug_huge.log' or 'truncate -s 0 logs/debug_huge.log'.",
                    "Do NOT delete the file with rm!",
                ],
                solution: ": > logs/debug_huge.log",
                setup: |ws| {
                    let huge_path = ws.join("logs").join("debug_huge.log");
                    let chunk = "2026-09-11 10:15:00 [DEBUG] TRACE 0xDEADBEEF core stack dump runaway recursion loop...\n";
                    fs::write(&huge_path, chunk.repeat(2000))?;
                    Ok(())
                },
                validate: |ws, _last_cmd| {
                    let huge_path = ws.join("logs").join("debug_huge.log");
                    let access_path = ws.join("logs").join("access.log");

                    if access_path.exists() && huge_path.exists() {
                        let size = fs::metadata(&huge_path).map(|m| m.len()).unwrap_or(1);
                        if size == 0 {
                            return ValidationResult::Success {
                                feedback: "Incident resolved! Runaway trace truncated to 0 bytes without removing application logs.".into(),
                            };
                        }
                    }
                    ValidationResult::Incomplete {
                        reason: "'logs/debug_huge.log' still has data. Truncate it to 0 bytes.".into(),
                    }
                },
            },
            Lesson {
                id: "inc_02",
                track_id: "incident",
                title: "Incident: Corrupted Server Configuration",
                description: "The web server crashed due to an invalid port in 'config/server.conf'. Fix it.",
                instructions: "'config/server.conf' has an invalid port line: 'port = NaN_PORT_CRASH'. Update it to 'port = 8080'.",
                hints: &[
                    "Use sed or edit the file to restore the valid port.",
                    "sed -i 's/NaN_PORT_CRASH/8080/' config/server.conf",
                ],
                solution: "sed -i 's/NaN_PORT_CRASH/8080/' config/server.conf",
                setup: |ws| {
                    let conf_path = ws.join("config").join("server.conf");
                    if let Ok(content) = fs::read_to_string(&conf_path) {
                        let corrupted = content.replace("port = 8080", "port = NaN_PORT_CRASH");
                        fs::write(&conf_path, corrupted)?;
                    }
                    Ok(())
                },
                validate: |ws, _last_cmd| {
                    let conf_path = ws.join("config").join("server.conf");
                    if let Ok(content) = fs::read_to_string(&conf_path)
                        && content.contains("port = 8080") && !content.contains("NaN_PORT_CRASH") {
                            return ValidationResult::Success {
                                feedback: "Server configuration fixed! Port successfully restored to 8080.".into(),
                            };
                        }
                    ValidationResult::Incomplete {
                        reason: "'config/server.conf' still contains 'NaN_PORT_CRASH' or is missing 'port = 8080'.".into(),
                    }
                },
            },
            Lesson {
                id: "inc_03",
                track_id: "incident",
                title: "Incident: Stale Lockfile Blocking Worker",
                description: "A crashed daemon left behind 'services/worker.lock'. Clear it to allow startup.",
                instructions: "The worker service fails to boot because 'services/worker.lock' is present. Remove 'services/worker.lock' so the service can restart.",
                hints: &[
                    "Use 'rm services/worker.lock'.",
                    "Do NOT delete 'services/worker.service'!",
                ],
                solution: "rm services/worker.lock",
                setup: |ws| {
                    fs::write(ws.join("services").join("worker.lock"), "44102\n")?;
                    Ok(())
                },
                validate: |ws, _last_cmd| {
                    let lock = ws.join("services").join("worker.lock");
                    let svc = ws.join("services").join("worker.service");
                    if !lock.exists() && svc.exists() {
                        ValidationResult::Success {
                            feedback: "Stale lockfile cleared! The worker service can now restart.".into(),
                        }
                    } else if !svc.exists() {
                        ValidationResult::Failed {
                            error: "Oops! 'services/worker.service' was accidentally deleted. Run 'tutor reset' to try again.".into(),
                        }
                    } else {
                        ValidationResult::Incomplete {
                            reason: "'services/worker.lock' still exists.".into(),
                        }
                    }
                },
            },
            Lesson {
                id: "inc_04",
                track_id: "incident",
                title: "Incident: Broken Shebang & Permissions",
                description: "CI/CD deployment failed with 'bad interpreter' and 'Permission denied'.",
                instructions: "1. Update the first line of 'scripts/deploy.sh' from '#!/usr/bin/broken_bash' to '#!/bin/sh'.\n2. Make 'scripts/deploy.sh' executable ('chmod +x scripts/deploy.sh').",
                hints: &[
                    "Fix shebang with sed: sed -i 's|#!/usr/bin/broken_bash|#!/bin/sh|' scripts/deploy.sh",
                    "Grant execute permissions: chmod +x scripts/deploy.sh",
                ],
                solution: "sed -i 's|#!/usr/bin/broken_bash|#!/bin/sh|' scripts/deploy.sh && chmod +x scripts/deploy.sh",
                setup: |ws| {
                    let deploy = ws.join("scripts").join("deploy.sh");
                    fs::write(
                        &deploy,
                        b"#!/usr/bin/broken_bash\n\
echo \"[DEPLOY] Running automated migration...\"\n\
echo \"[DEPLOY] Deployment successful!\"\n",
                    )?;
                    #[cfg(unix)]
                    {
                        let mut perms = fs::metadata(&deploy)?.permissions();
                        perms.set_mode(0o644);
                        fs::set_permissions(&deploy, perms)?;
                    }
                    Ok(())
                },
                validate: |ws, _last_cmd| {
                    let deploy = ws.join("scripts").join("deploy.sh");
                    if let Ok(content) = fs::read_to_string(&deploy) {
                        let valid_shebang = content.starts_with("#!/bin/sh")
                            || content.starts_with("#!/usr/bin/env bash")
                            || content.starts_with("#!/bin/bash");

                        #[cfg(unix)]
                        let is_exec = fs::metadata(&deploy)
                            .map(|m| m.permissions().mode() & 0o111 != 0)
                            .unwrap_or(false);
                        #[cfg(not(unix))]
                        let is_exec = true;

                        if valid_shebang && is_exec {
                            ValidationResult::Success {
                                feedback: "Incident resolved! Shebang repaired and deploy script is executable.".into(),
                            }
                        } else if !valid_shebang {
                            ValidationResult::Incomplete {
                                reason: "'scripts/deploy.sh' does not have a valid shebang (e.g. '#!/bin/sh').".into(),
                            }
                        } else {
                            ValidationResult::Incomplete {
                                reason: "'scripts/deploy.sh' is not yet executable (run 'chmod +x scripts/deploy.sh').".into(),
                            }
                        }
                    } else {
                        ValidationResult::Incomplete {
                            reason: "'scripts/deploy.sh' does not exist.".into(),
                        }
                    }
                },
            },
        ],
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TRACK 6: SRE & SAFETY (Services, Diagnostics & Rollback)
// ─────────────────────────────────────────────────────────────────────────────
fn build_sre_track() -> LessonTrack {
    LessonTrack {
        id: "sre",
        title: "The SRE (Services & Resilience)",
        description: "Control microservice lifecycles, preview changes with whatif, and diagnose outages.",
        lessons: vec![
            Lesson {
                id: "sre_01",
                track_id: "sre",
                title: "Microservice Lifecycle & Status",
                description: "Modern cloud platforms manage microservices. ShellPilot includes a built-in service manager. Start the payment gateway web service with 'service start web' and verify with 'service status web'.",
                instructions: "Run 'service start web' to boot the payment microservice.",
                hints: &[
                    "Start the service: service start web",
                    "Check status afterwards: service status web",
                ],
                solution: "service start web",
                setup: |_ws| Ok(()),
                validate: |_ws, last_cmd| {
                    if let Some(cmd) = last_cmd {
                        let c = cmd.trim();
                        if c.contains("service start web") || c.contains("service restart web") {
                            return ValidationResult::Success {
                                feedback: "Payment web gateway booted successfully! Listening on port 8080.".into(),
                            };
                        }
                    }
                    ValidationResult::Incomplete {
                        reason: "Run 'service start web' to boot the service.".into(),
                    }
                },
            },
            Lesson {
                id: "sre_02",
                track_id: "sre",
                title: "Blast-Radius Preview ('whatif')",
                description: "Destructive commands like 'rm -rf' are dangerous in production. ShellPilot provides 'whatif' to simulate any command, displaying targeted files, size, and blast radius without modifying disk.",
                instructions: "Run a dry-run preview of deleting logs: whatif 'rm -rf logs/*.log'",
                hints: &[
                    "Execute: whatif 'rm -rf logs/*.log'",
                    "Notice how whatif details deleted files without altering disk.",
                ],
                solution: "whatif 'rm -rf logs/*.log'",
                setup: |_ws| Ok(()),
                validate: |_ws, last_cmd| {
                    if let Some(cmd) = last_cmd
                        && cmd.contains("whatif") && cmd.contains("logs") {
                            return ValidationResult::Success {
                                feedback: "Dry-run analysis complete! You previewed the deletion safely without modifying disk.".into(),
                            };
                        }
                    ValidationResult::Incomplete {
                        reason: "Run whatif 'rm -rf logs/*.log' to preview the blast radius.".into(),
                    }
                },
            },
            Lesson {
                id: "sre_03",
                track_id: "sre",
                title: "Contextual Recovery with 'doctor'",
                description: "When commands fail, ShellPilot's diagnostic engine 'doctor' analyzes error outputs, file permissions, and directory structures to recommend exact remedies.",
                instructions: "Simulate a typo by running 'cat app/server', observe the error, and then type 'doctor'.",
                hints: &[
                    "First trigger the missing file error: cat app/server",
                    "Then run: doctor",
                ],
                solution: "cat app/server; doctor",
                setup: |_ws| Ok(()),
                validate: |_ws, last_cmd| {
                    if let Some(cmd) = last_cmd
                        && cmd.contains("doctor") {
                            return ValidationResult::Success {
                                feedback: "Doctor consultation complete! The diagnostic engine accurately spotted the missing .py extension.".into(),
                            };
                        }
                    ValidationResult::Incomplete {
                        reason: "Run 'doctor' to consult the diagnostic engine.".into(),
                    }
                },
            },
            Lesson {
                id: "sre_04",
                track_id: "sre",
                title: "Live API Inspection with 'curl'",
                description: "Once services are running, test HTTP endpoints using ShellPilot's built-in 'curl'. Inspect the health check endpoint of your simulated web service.",
                instructions: "Run 'curl http://localhost:8080/health' (ensure 'service start web' has been run).",
                hints: &[
                    "Ensure service is running: service start web",
                    "Then query health endpoint: curl http://localhost:8080/health",
                ],
                solution: "curl http://localhost:8080/health",
                setup: |_ws| Ok(()),
                validate: |_ws, last_cmd| {
                    if let Some(cmd) = last_cmd
                        && cmd.contains("curl") && (cmd.contains("8080") || cmd.contains("health") || cmd.contains("customers")) {
                            return ValidationResult::Success {
                                feedback: "HTTP 200 response received! API telemetry verified online.".into(),
                            };
                        }
                    ValidationResult::Incomplete {
                        reason: "Run 'curl http://localhost:8080/health' to test the microservice API.".into(),
                    }
                },
            },
        ],
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TRACK 7: DATA ALCHEMIST (Structured |> Pipelines)
// ─────────────────────────────────────────────────────────────────────────────
fn build_data_alchemist_track() -> LessonTrack {
    LessonTrack {
        id: "data",
        title: "The Data Alchemist (Structured |> Pipelines)",
        description: "Master ShellPilot's unique structured pipe operator |> for JSON/YAML/JSONL object manipulation.",
        lessons: vec![
            Lesson {
                id: "data_01",
                track_id: "data",
                title: "Structured Field Projection",
                description: "Unlike plain Unix pipes that pass raw byte streams, ShellPilot's '|>' operator understands structured data. Extract the '.name' property of each item in 'data/inventory.jsonl' and save to 'data/names.txt'.",
                instructions: "Run: cat data/inventory.jsonl |> .name > data/names.txt",
                hints: &[
                    "Use the structured pipe operator: |>",
                    "Syntax: cat data/inventory.jsonl |> .name > data/names.txt",
                ],
                solution: "cat data/inventory.jsonl |> .name > data/names.txt",
                setup: |_ws| Ok(()),
                validate: |ws, _last_cmd| {
                    let out_file = ws.join("data/names.txt");
                    if let Ok(content) = fs::read_to_string(&out_file)
                        && content.contains("Rack Server 1U") && content.contains("Gigabit Switch") {
                            return ValidationResult::Success {
                                feedback: "Structured projection successful! Extracted product names into data/names.txt.".into(),
                            };
                        }
                    ValidationResult::Incomplete {
                        reason: "Output file 'data/names.txt' does not contain the projected names. Run 'cat data/inventory.jsonl |> .name > data/names.txt'.".into(),
                    }
                },
            },
            Lesson {
                id: "data_02",
                track_id: "data",
                title: "Object Filtering Streams",
                description: "Filter structured objects on the fly. Select items from 'data/inventory.jsonl' where '.price == 350.0' or '.qty == 14' and save to 'data/filtered.jsonl'.",
                instructions: "Run: cat data/inventory.jsonl |> filter (.qty == 14) > data/filtered.jsonl",
                hints: &[
                    "Use the filter clause: |> filter (.qty == 14)",
                    "Full command: cat data/inventory.jsonl |> filter (.qty == 14) > data/filtered.jsonl",
                ],
                solution: "cat data/inventory.jsonl |> filter (.qty == 14) > data/filtered.jsonl",
                setup: |_ws| Ok(()),
                validate: |ws, _last_cmd| {
                    let out_file = ws.join("data/filtered.jsonl");
                    if let Ok(content) = fs::read_to_string(&out_file)
                        && content.contains("Rack Server 1U") && !content.contains("Ethernet Cable") {
                            return ValidationResult::Success {
                                feedback: "Stream filtered successfully! Only matching objects were retained.".into(),
                            };
                        }
                    ValidationResult::Incomplete {
                        reason: "'data/filtered.jsonl' does not contain the filtered item. Run 'cat data/inventory.jsonl |> filter (.qty == 14) > data/filtered.jsonl'.".into(),
                    }
                },
            },
            Lesson {
                id: "data_03",
                track_id: "data",
                title: "Stream Truncation with 'take'",
                description: "Limit stream output to the top N records using '|> take N'. Take the first 2 records from 'data/inventory.jsonl' into 'data/top2.jsonl'.",
                instructions: "Run: cat data/inventory.jsonl |> take 2 > data/top2.jsonl",
                hints: &[
                    "Syntax: cat data/inventory.jsonl |> take 2 > data/top2.jsonl",
                ],
                solution: "cat data/inventory.jsonl |> take 2 > data/top2.jsonl",
                setup: |_ws| Ok(()),
                validate: |ws, _last_cmd| {
                    let out_file = ws.join("data/top2.jsonl");
                    if let Ok(content) = fs::read_to_string(&out_file) {
                        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
                        if lines.len() == 2 && content.contains("Rack Server 1U") {
                            return ValidationResult::Success {
                                feedback: "Stream truncated to exactly 2 records!".into(),
                            };
                        }
                    }
                    ValidationResult::Incomplete {
                        reason: "'data/top2.jsonl' should contain exactly the first 2 JSON lines.".into(),
                    }
                },
            },
            Lesson {
                id: "data_04",
                track_id: "data",
                title: "Format Transmutation (JSON -> YAML)",
                description: "Transmute structured formats instantly. Convert 'app/config.json' to clean YAML and save to 'config/app_config.yaml'.",
                instructions: "Run: cat app/config.json |> yaml > config/app_config.yaml",
                hints: &[
                    "Syntax: cat app/config.json |> yaml > config/app_config.yaml",
                ],
                solution: "cat app/config.json |> yaml > config/app_config.yaml",
                setup: |_ws| Ok(()),
                validate: |ws, _last_cmd| {
                    let out_file = ws.join("config/app_config.yaml");
                    if let Ok(content) = fs::read_to_string(&out_file)
                        && content.contains("appName:") && content.contains("database:") {
                            return ValidationResult::Success {
                                feedback: "Format transmutation complete! JSON converted to YAML seamlessly.".into(),
                            };
                        }
                    ValidationResult::Incomplete {
                        reason: "'config/app_config.yaml' does not contain valid YAML. Run 'cat app/config.json |> yaml > config/app_config.yaml'.".into(),
                    }
                },
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tutor_lists_tracks_and_lessons() {
        let tutor = TutorEngine::new(None);
        let overview = tutor.list_tracks_overview();
        assert!(overview.contains("TRACK: Cadet"));
        assert!(overview.contains("TRACK: The Plumber"));
        assert!(overview.contains("TRACK: The Guardian"));
        assert!(overview.contains("TRACK: The Detective"));
        assert!(overview.contains("TRACK: Incident Responder"));
        assert!(overview.contains("nav_01"));
        assert!(overview.contains("nav_04"));
        assert!(overview.contains("pipe_01"));
        assert!(overview.contains("pipe_04"));
        assert!(overview.contains("perm_01"));
        assert!(overview.contains("perm_03"));
        assert!(overview.contains("find_01"));
        assert!(overview.contains("find_03"));
        assert!(overview.contains("inc_01"));
        assert!(overview.contains("inc_04"));
    }

    #[test]
    fn tutor_starts_lesson_and_sets_up_files() {
        let mut tutor = TutorEngine::new(None);
        let temp = std::env::temp_dir().join(format!("shellpilot_test_tutor_start_{}", std::process::id()));
        fs::create_dir_all(&temp).unwrap();

        let briefing = tutor.start_lesson("nav_01", &temp).unwrap();
        assert!(briefing.contains("Lesson nav_01"));
        assert!(temp.join("README.md").exists());
        assert!(temp.join("app").join("server.py").exists());
        assert!(temp.join("config").join("server.conf").exists());

        // Validate incomplete initially
        let val = tutor.evaluate_current(&temp, None).unwrap();
        assert!(matches!(val, ValidationResult::Incomplete { .. }));

        // Simulate completion: create docs/overview.txt
        let docs = temp.join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::copy(temp.join("README.md"), docs.join("overview.txt")).unwrap();

        // Validate success
        let val = tutor.evaluate_current(&temp, None).unwrap();
        assert!(matches!(val, ValidationResult::Success { .. }));

        fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn tutor_hints_cycle_correctly() {
        let mut tutor = TutorEngine::new(None);
        let temp = std::env::temp_dir().join(format!("shellpilot_test_tutor_hints_{}", std::process::id()));
        fs::create_dir_all(&temp).unwrap();
        tutor.start_lesson("nav_02", &temp).unwrap();

        let hint1 = tutor.next_hint().unwrap();
        assert!(hint1.contains("Hint (1/"));

        let hint2 = tutor.next_hint().unwrap();
        assert!(hint2.contains("Hint (2/"));

        let hint3 = tutor.next_hint().unwrap();
        assert!(hint3.contains("You have seen all hints"));

        fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn incident_track_validation_works() {
        let mut tutor = TutorEngine::new(None);
        let temp = std::env::temp_dir().join(format!("shellpilot_test_incident_{}", std::process::id()));
        fs::create_dir_all(&temp).unwrap();
        tutor.start_lesson("inc_01", &temp).unwrap();

        let trace = temp.join("logs").join("debug_huge.log");
        assert!(trace.exists());
        assert!(fs::metadata(&trace).unwrap().len() > 0);

        // Truncate file
        fs::write(&trace, "").unwrap();

        let val = tutor.evaluate_current(&temp, None).unwrap();
        assert!(matches!(val, ValidationResult::Success { .. }));

        fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn plumber_track_validation_works() {
        let mut tutor = TutorEngine::new(None);
        let temp = std::env::temp_dir().join(format!("shellpilot_test_plumber_{}", std::process::id()));
        fs::create_dir_all(&temp).unwrap();
        tutor.start_lesson("pipe_01", &temp).unwrap();

        let audit = temp.join("logs").join("audit.log");
        assert!(!audit.exists());

        // Write required lines
        fs::write(&audit, "SYSTEM AUDIT INITIALIZED\nAUDIT RUNNER: root\n").unwrap();

        let val = tutor.evaluate_current(&temp, None).unwrap();
        assert!(matches!(val, ValidationResult::Success { .. }));

        fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn guardian_track_validation_works() {
        let mut tutor = TutorEngine::new(None);
        let temp = std::env::temp_dir().join(format!("shellpilot_test_guardian_{}", std::process::id()));
        fs::create_dir_all(&temp).unwrap();
        tutor.start_lesson("perm_01", &temp).unwrap();

        let deploy = temp.join("scripts").join("deploy.sh");
        assert!(deploy.exists());

        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&deploy).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&deploy, perms).unwrap();

            let val = tutor.evaluate_current(&temp, None).unwrap();
            assert!(matches!(val, ValidationResult::Success { .. }));
        }

        fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn detective_track_validation_works() {
        let mut tutor = TutorEngine::new(None);
        let temp = std::env::temp_dir().join(format!("shellpilot_test_detective_{}", std::process::id()));
        fs::create_dir_all(&temp).unwrap();
        tutor.start_lesson("find_03", &temp).unwrap();

        let out = temp.join("data").join("customer_emails.txt");
        assert!(!out.exists());

        let emails = "alice@acme.com\nbob@techcorp.io\ncharlie@startup.dev\ndiana@themyscira.gov\nevan@devops.co\nfiona@shamrock.org\ngeorge@cloudscale.net\nhannah@potion.co.uk\n";
        fs::write(&out, emails).unwrap();

        let val = tutor.evaluate_current(&temp, None).unwrap();
        assert!(matches!(val, ValidationResult::Success { .. }));

        fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn sre_track_validation_works() {
        let mut tutor = TutorEngine::new(None);
        let temp = std::env::temp_dir().join(format!("shellpilot_test_sre_{}", std::process::id()));
        fs::create_dir_all(&temp).unwrap();

        tutor.start_lesson("sre_01", &temp).unwrap();
        let val1 = tutor.evaluate_current(&temp, Some("service start web")).unwrap();
        assert!(matches!(val1, ValidationResult::Success { .. }));

        tutor.start_lesson("sre_02", &temp).unwrap();
        let val2 = tutor.evaluate_current(&temp, Some("whatif 'rm -rf logs/*.log'")).unwrap();
        assert!(matches!(val2, ValidationResult::Success { .. }));

        fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn data_track_validation_works() {
        let mut tutor = TutorEngine::new(None);
        let temp = std::env::temp_dir().join(format!("shellpilot_test_data_{}", std::process::id()));
        fs::create_dir_all(&temp).unwrap();

        tutor.start_lesson("data_01", &temp).unwrap();
        let out = temp.join("data/names.txt");
        fs::write(&out, "Rack Server 1U\nGigabit Switch 24p\n").unwrap();
        let val = tutor.evaluate_current(&temp, None).unwrap();
        assert!(matches!(val, ValidationResult::Success { .. }));

        fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn test_all_26_lessons_lifecycle_and_metadata() {
        let tracks = build_all_tracks();
        assert_eq!(tracks.len(), 7, "Must have exactly 7 tracks");
        let total_lessons: usize = tracks.iter().map(|t| t.lessons.len()).sum();
        assert_eq!(total_lessons, 26, "Must have exactly 26 lessons across tracks");

        let temp = std::env::temp_dir().join(format!("shellpilot_test_all_26_{}", std::process::id()));
        fs::create_dir_all(&temp).unwrap();

        let mut tutor = TutorEngine::new(None);
        for track in &tracks {
            assert!(!track.id.is_empty());
            assert!(!track.title.is_empty());
            assert!(!track.description.is_empty());

            for lesson in &track.lessons {
                assert!(!lesson.id.is_empty());
                assert!(!lesson.title.is_empty());
                assert!(!lesson.description.is_empty());
                assert!(!lesson.instructions.is_empty());
                assert!(!lesson.solution.is_empty());
                assert!(!lesson.hints.is_empty(), "Lesson {} must have hints", lesson.id);

                let briefing = tutor
                    .start_lesson(lesson.id, &temp)
                    .unwrap_or_else(|e| panic!("Failed to start {}: {e}", lesson.id));
                assert!(briefing.contains(lesson.id));

                // Should initially be Incomplete (not yet solved)
                let initial_eval = tutor
                    .evaluate_current(&temp, None)
                    .unwrap_or_else(|| panic!("Failed to eval {}", lesson.id));
                assert!(
                    matches!(initial_eval, ValidationResult::Incomplete { .. }),
                    "Lesson {} should initially be incomplete, got {:?}",
                    lesson.id,
                    initial_eval
                );

                // Solved solution string must match get_solution
                let sol_str = tutor
                    .get_solution()
                    .unwrap_or_else(|| panic!("No solution for {}", lesson.id));
                assert!(sol_str.contains(lesson.solution));

                // Reset works
                tutor
                    .reset_lesson(&temp)
                    .unwrap_or_else(|e| panic!("Reset failed for {}: {e}", lesson.id));
            }
        }

        fs::remove_dir_all(&temp).ok();
    }
}
