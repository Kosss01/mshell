use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static SANDBOX_COUNTER: AtomicU64 = AtomicU64::new(1);

use crate::effects::{self, Effect};
use crate::parser::{self, Ast};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxSnapshot {
    pub id: usize,
    pub timestamp: u64,
    pub command: String,
    pub snapshot_path: PathBuf,
}

#[derive(Debug)]
pub struct SandboxManager {
    pub sandbox_root: PathBuf,
    pub workspace: PathBuf,
    #[allow(dead_code)]
    pub home: PathBuf,
    pub snapshot_dir: PathBuf,
    pub snapshots: Vec<SandboxSnapshot>,
    pub max_snapshots: usize,
    pub next_snapshot_id: usize,
}

impl SandboxManager {
    /// Create a new isolated sandbox manager under a temporary directory.
    pub fn new() -> io::Result<Self> {
        let count = SANDBOX_COUNTER.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        let sandbox_root = std::env::temp_dir().join(format!("shellpilot_sandbox_{}_{}_{}", pid, timestamp, count));

        let workspace = sandbox_root.join("workspace");
        let home = sandbox_root.join("home");
        let snapshot_dir = sandbox_root.join(".snapshots");

        fs::create_dir_all(&workspace)?;
        fs::create_dir_all(&home)?;
        fs::create_dir_all(&snapshot_dir)?;
        populate_sandbox_ecosystem(&workspace)?;

        Ok(Self {
            sandbox_root,
            workspace,
            home,
            snapshot_dir,
            snapshots: Vec::new(),
            max_snapshots: 10,
            next_snapshot_id: 1,
        })
    }

    /// Create a sandbox manager with a custom root directory (useful for unit tests).
    #[allow(dead_code)]
    pub fn with_root(sandbox_root: PathBuf) -> io::Result<Self> {
        let workspace = sandbox_root.join("workspace");
        let home = sandbox_root.join("home");
        let snapshot_dir = sandbox_root.join(".snapshots");

        fs::create_dir_all(&workspace)?;
        fs::create_dir_all(&home)?;
        fs::create_dir_all(&snapshot_dir)?;
        populate_sandbox_ecosystem(&workspace)?;

        Ok(Self {
            sandbox_root,
            workspace,
            home,
            snapshot_dir,
            snapshots: Vec::new(),
            max_snapshots: 10,
            next_snapshot_id: 1,
        })
    }

    /// Check if an accessed path is safely confined inside the sandbox or system read paths.
    pub fn is_path_jailed(&self, path: &Path, is_write: bool) -> bool {
        // In writes, absolute paths MUST be inside sandbox_root
        if path.is_absolute() {
            if path.starts_with(&self.sandbox_root) {
                return true;
            }
            // Writes outside sandbox root are forbidden
            if is_write {
                return false;
            }
            // For reads, allowing standard system directories (/bin, /usr, /lib) is normal for commands to work
            let path_str = path.to_string_lossy();
            if path_str.starts_with("/bin")
                || path_str.starts_with("/usr")
                || path_str.starts_with("/lib")
                || path_str.starts_with("/etc/alternatives")
            {
                return true;
            }
            return false;
        }

        // Relative paths should not escape the sandbox root when combined with workspace
        let resolved = self.workspace.join(path);
        // Normalize any ".." components
        let mut normalized = PathBuf::new();
        for component in resolved.components() {
            match component {
                std::path::Component::ParentDir => {
                    normalized.pop();
                }
                c => normalized.push(c),
            }
        }

        normalized.starts_with(&self.sandbox_root)
    }

    /// Create an automatic pre-command snapshot of the workspace before a modifying action.
    pub fn pre_command_snapshot(&mut self, command: &str) -> io::Result<usize> {
        let id = self.next_snapshot_id;
        self.next_snapshot_id += 1;

        let snapshot_path = self.snapshot_dir.join(format!("snap_{}", id));
        copy_dir_all(&self.workspace, &snapshot_path)?;

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.snapshots.push(SandboxSnapshot {
            id,
            timestamp,
            command: command.to_string(),
            snapshot_path,
        });

        // Prune oldest snapshots beyond limit
        while self.snapshots.len() > self.max_snapshots {
            let oldest = self.snapshots.remove(0);
            fs::remove_dir_all(&oldest.snapshot_path).ok();
        }

        Ok(id)
    }

    /// Revert the workspace to the state prior to the most recent command.
    pub fn undo(&mut self) -> io::Result<String> {
        let snapshot = match self.snapshots.pop() {
            Some(snap) => snap,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "No previous snapshots to undo.",
                ));
            }
        };

        // Clear current workspace contents
        if self.workspace.exists() {
            for entry in fs::read_dir(&self.workspace)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() {
                    fs::remove_dir_all(&path)?;
                } else {
                    fs::remove_file(&path)?;
                }
            }
        } else {
            fs::create_dir_all(&self.workspace)?;
        }

        // Copy snapshot contents back into workspace
        copy_dir_all(&snapshot.snapshot_path, &self.workspace)?;

        // Clean up the used snapshot folder
        fs::remove_dir_all(&snapshot.snapshot_path).ok();

        Ok(format!(
            "⏪ Restored sandbox state prior to: '{}' (Checkpoint #{})",
            snapshot.command, snapshot.id
        ))
    }

    /// Compare current workspace against the latest snapshot and produce a formatted diff.
    pub fn diff_previous_snapshot(&self) -> io::Result<String> {
        let snapshot = match self.snapshots.last() {
            Some(snap) => snap,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "No previous snapshots to diff. Run a modifying command first.",
                ));
            }
        };

        let mut out = String::new();
        out.push_str("\n\x1b[1;36m╔══════════════════════════════════════════════════════════════════════╗\x1b[0m\n");
        out.push_str(&format!(
            "\x1b[1;36m║  🔍 TIME-TRAVEL DIFF: CHANGES SINCE CHECKPOINT #{:<3}                  ║\x1b[0m\n",
            snapshot.id
        ));
        out.push_str("\x1b[1;36m╚══════════════════════════════════════════════════════════════════════╝\x1b[0m\n\n");
        out.push_str(&format!("Command triggering snapshot: \x1b[1;33m{}\x1b[0m\n\n", snapshot.command));

        let mut changes = 0;

        let snap_files = collect_file_paths(&snapshot.snapshot_path)?;
        let work_files = collect_file_paths(&self.workspace)?;

        // Deleted in workspace
        for rel in &snap_files {
            if !work_files.contains(rel) {
                changes += 1;
                out.push_str(&format!("  \x1b[1;31m❌ Deleted :\x1b[0m {}\n", rel.display()));
            }
        }

        // Added in workspace
        for rel in &work_files {
            if !snap_files.contains(rel) {
                changes += 1;
                out.push_str(&format!("  \x1b[1;32m➕ Added   :\x1b[0m {}\n", rel.display()));
            }
        }

        // Modified files
        for rel in &snap_files {
            if work_files.contains(rel) {
                let snap_p = snapshot.snapshot_path.join(rel);
                let work_p = self.workspace.join(rel);
                if let (Ok(snap_bytes), Ok(work_bytes)) = (fs::read(&snap_p), fs::read(&work_p))
                    && snap_bytes != work_bytes {
                        changes += 1;
                        out.push_str(&format!("  \x1b[1;33m📝 Modified:\x1b[0m {}\n", rel.display()));
                        if let (Ok(s1), Ok(s2)) = (String::from_utf8(snap_bytes), String::from_utf8(work_bytes)) {
                            let l1: Vec<&str> = s1.lines().collect();
                            let l2: Vec<&str> = s2.lines().collect();
                            for line in l1.iter().filter(|l| !l2.contains(l)).take(5) {
                                out.push_str(&format!("     \x1b[0;31m- {}\x1b[0m\n", line));
                            }
                            for line in l2.iter().filter(|l| !l1.contains(l)).take(5) {
                                out.push_str(&format!("     \x1b[0;32m+ {}\x1b[0m\n", line));
                            }
                        }
                    }
            }
        }

        if changes == 0 {
            out.push_str("  (No filesystem changes detected between workspace and snapshot.)\n");
        } else {
            out.push_str("\n\x1b[0;90m💡 Type 'undo' to revert all changes back to this checkpoint.\x1b[0m\n");
        }

        Ok(out)
    }

    /// Clean up the entire sandbox directory.
    pub fn cleanup(&mut self) -> io::Result<()> {
        if self.sandbox_root.exists() {
            fs::remove_dir_all(&self.sandbox_root)?;
        }
        self.snapshots.clear();
        Ok(())
    }
}

impl Drop for SandboxManager {
    fn drop(&mut self) {
        // Best-effort cleanup upon drop
        self.cleanup().ok();
    }
}

/// Helper function to recursively copy a directory.
fn copy_dir_all(src: &Path, dst: &Path) -> io::Result<()> {
    if !dst.exists() {
        fs::create_dir_all(dst)?;
    }
    if !src.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());

        if ty.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Populate a rich virtual server environment for the learner to explore and manipulate.
pub fn populate_sandbox_ecosystem(workspace: &Path) -> io::Result<()> {
    // 1. App directory
    let app_dir = workspace.join("app");
    fs::create_dir_all(&app_dir)?;
    fs::write(
        app_dir.join("server.py"),
        b"#!/usr/bin/env python3\n\
import json\n\
from http.server import HTTPServer, BaseHTTPRequestHandler\n\n\
PORT = 8080\n\n\
class Handler(BaseHTTPRequestHandler):\n\
    def do_GET(self):\n\
        self.send_response(200)\n\
        self.send_header('Content-type', 'application/json')\n\
        self.end_headers()\n\
        self.wfile.write(json.dumps({\"status\": \"healthy\", \"version\": \"2.4.1\"}).encode())\n\n\
if __name__ == '__main__':\n\
    print(f'Starting payment server on port {PORT}...')\n\
    server = HTTPServer(('0.0.0.0', PORT), Handler)\n\
    server.serve_forever()\n",
    )?;

    fs::write(
        app_dir.join("config.json"),
        b"{\n\
  \"appName\": \"PaymentGateway\",\n\
  \"version\": \"2.4.1\",\n\
  \"environment\": \"staging\",\n\
  \"database\": {\n\
    \"host\": \"10.0.4.15\",\n\
    \"port\": 5432,\n\
    \"poolSize\": 20\n\
  },\n\
  \"features\": {\n\
    \"rateLimiting\": true,\n\
    \"debugLogging\": false\n\
  }\n\
}\n",
    )?;

    let templates_dir = app_dir.join("templates");
    fs::create_dir_all(&templates_dir)?;
    fs::write(
        templates_dir.join("index.html"),
        b"<!DOCTYPE html>\n<html>\n<head><title>Payment Gateway</title></head>\n<body>\n  <h1>Service Status: Online</h1>\n  <p>Environment: Staging (v2.4.1)</p>\n</body>\n</html>\n",
    )?;

    // 2. Config directory
    let config_dir = workspace.join("config");
    fs::create_dir_all(&config_dir)?;
    fs::write(
        config_dir.join("server.conf"),
        b"[server]\n\
host = 0.0.0.0\n\
port = 8080\n\
max_connections = 1024\n\
keepalive_timeout = 65\n\n\
[security]\n\
ssl_enabled = true\n\
cert_file = /etc/ssl/certs/server.crt\n\
key_file = /etc/ssl/private/server.key\n\n\
[logging]\n\
log_level = INFO\n\
log_file = logs/app.log\n",
    )?;

    fs::write(
        config_dir.join("database.yaml"),
        b"database:\n\
  driver: postgresql\n\
  host: db-primary.internal.net\n\
  port: 5432\n\
  name: production_db\n\
  user: pg_app_user\n\
  ssl_mode: require\n\
  max_idle: 10\n\
  max_open: 50\n",
    )?;

    fs::write(
        config_dir.join("settings.env"),
        b"NODE_ENV=production\n\
PORT=8080\n\
CACHE_TTL=3600\n\
REDIS_URL=redis://127.0.0.1:6379\n\
ENABLE_METRICS=true\n\
API_SECRET_KEY=prod_live_sec_9938192837482910\n",
    )?;

    // 3. Logs directory
    let logs_dir = workspace.join("logs");
    fs::create_dir_all(&logs_dir)?;
    fs::write(
        logs_dir.join("access.log"),
        b"192.168.1.100 - - [11/Sep/2026:10:14:20 +0000] \"GET /index.html HTTP/1.1\" 200 4523\n\
192.168.1.101 - - [11/Sep/2026:10:14:21 +0000] \"POST /api/v1/auth HTTP/1.1\" 200 128\n\
10.0.0.55 - - [11/Sep/2026:10:14:22 +0000] \"GET /assets/style.css HTTP/1.1\" 200 8920\n\
172.16.0.4 - - [11/Sep/2026:10:14:23 +0000] \"GET /favicon.ico HTTP/1.1\" 404 153\n\
192.168.1.105 - - [11/Sep/2026:10:14:25 +0000] \"GET /api/v1/users HTTP/1.1\" 200 2400\n\
10.0.0.55 - - [11/Sep/2026:10:14:26 +0000] \"POST /api/v1/payment HTTP/1.1\" 500 230\n\
172.16.0.12 - - [11/Sep/2026:10:14:28 +0000] \"GET /missing-page HTTP/1.1\" 404 153\n\
192.168.1.100 - - [11/Sep/2026:10:14:30 +0000] \"GET /dashboard HTTP/1.1\" 200 10240\n\
10.0.0.99 - - [11/Sep/2026:10:14:31 +0000] \"GET /old-api HTTP/1.1\" 404 153\n\
172.16.0.4 - - [11/Sep/2026:10:14:35 +0000] \"POST /api/v1/checkout HTTP/1.1\" 200 512\n",
    )?;

    fs::write(
        logs_dir.join("error.log"),
        b"2026-09-11 10:14:26 [ERROR] PaymentGateway::charge: Database connection timed out (host: 10.0.4.15:5432)\n\
2026-09-11 10:14:27 [CRITICAL] RedisCache::ping: Connection reset by peer\n\
2026-09-11 10:14:30 [WARNING] RateLimiter: Client 10.0.0.99 exceeded 100 req/min threshold\n\
2026-09-11 10:14:35 [INFO] PaymentGateway::charge: Retry succeeded for txn_992104\n",
    )?;

    fs::write(
        logs_dir.join("auth.log"),
        b"Sep 11 10:01:02 auth-server sshd[12401]: Accepted publickey for alice from 192.168.1.100 port 52310 ssh2\n\
Sep 11 10:04:15 auth-server sshd[12450]: Failed password for invalid user root from 203.0.113.45 port 41200 ssh2\n\
Sep 11 10:04:18 auth-server sshd[12452]: Failed password for invalid user admin from 203.0.113.45 port 41202 ssh2\n\
Sep 11 10:06:22 auth-server sshd[12480]: Failed password for user bob from 192.168.1.105 port 55100 ssh2\n\
Sep 11 10:06:40 auth-server sshd[12485]: Accepted password for user bob from 192.168.1.105 port 55100 ssh2\n",
    )?;

    // 4. Data directory
    let data_dir = workspace.join("data");
    fs::create_dir_all(&data_dir)?;
    fs::write(
        data_dir.join("customers.csv"),
        b"id,name,email,plan,country,status\n\
1,Alice Smith,alice@acme.com,Enterprise,US,active\n\
2,Bob Jones,bob@techcorp.io,Professional,UK,active\n\
3,Charlie Brown,charlie@startup.dev,Starter,CA,suspended\n\
4,Diana Prince,diana@themyscira.gov,Enterprise,US,active\n\
5,Evan Wright,evan@devops.co,Professional,DE,active\n\
6,Fiona Gallagher,fiona@shamrock.org,Starter,IE,canceled\n\
7,George Clark,george@cloudscale.net,Enterprise,UK,active\n\
8,Hannah Abbott,hannah@potion.co.uk,Professional,UK,active\n",
    )?;

    fs::write(
        data_dir.join("inventory.jsonl"),
        b"{\"sku\": \"SRV-01\", \"name\": \"Rack Server 1U\", \"qty\": 14, \"price\": 1200.0}\n\
{\"sku\": \"SW-24\", \"name\": \"Gigabit Switch 24p\", \"qty\": 28, \"price\": 350.0}\n\
{\"sku\": \"CAB-CAT6\", \"name\": \"Ethernet Cable 2m\", \"qty\": 350, \"price\": 4.5}\n\
{\"sku\": \"PWR-850\", \"name\": \"Power Supply 850W\", \"qty\": 42, \"price\": 110.0}\n",
    )?;

    // Initialize production.db SQLite database
    let _ = crate::server_sim::ensure_production_db(&data_dir.join("production.db"));

    // Config SSL & blocklist
    let ssl_dir = config_dir.join("ssl");
    fs::create_dir_all(&ssl_dir)?;
    fs::write(
        ssl_dir.join("cert.pem"),
        b"-----BEGIN CERTIFICATE-----\n\
MIIDITCCAomgAwIBAgIUStaleExpiredCert20250911000000Z\n\
Issuer: CN=Acme Staging Root CA, O=FlightSim, C=US\n\
Validity: NotAfter: Sep 01 00:00:00 2025 GMT [STATUS: EXPIRED]\n\
Subject: CN=gateway.payment.internal\n\
-----END CERTIFICATE-----\n",
    )?;
    fs::write(
        config_dir.join("blocklist.conf"),
        b"# Production Gateway IP Blocklist\n# Format: <ip_address>\n",
    )?;

    // 5. Scripts directory
    let scripts_dir = workspace.join("scripts");
    fs::create_dir_all(&scripts_dir)?;
    let backup_script = scripts_dir.join("backup.sh");
    fs::write(
        &backup_script,
        b"#!/bin/sh\n\
echo \"[BACKUP] Starting automated archive at $(date)...\"\ntar -czf /tmp/backup_data.tar.gz config/ data/\n\
echo \"[BACKUP] Finished successfully.\"\n",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&backup_script, fs::Permissions::from_mode(0o755)).ok();
    }

    let healthcheck = scripts_dir.join("healthcheck.sh");
    fs::write(
        &healthcheck,
        b"#!/bin/sh\n\
echo \"[HEALTHCHECK] Database: OK\"\n\
echo \"[HEALTHCHECK] Cache: OK\"\n\
echo \"[HEALTHCHECK] Disk: 84% free\"\n\
echo \"[WARNING] Memory swap usage at 78%\" >&2\n",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&healthcheck, fs::Permissions::from_mode(0o755)).ok();
    }

    // 6. Services directory
    let services_dir = workspace.join("services");
    fs::create_dir_all(&services_dir)?;
    fs::write(
        services_dir.join("web.service"),
        b"[Unit]\n\
Description=Payment Web Gateway\n\
After=network.target\n\n\
[Service]\n\
ExecStart=/usr/bin/python3 app/server.py\n\
Restart=always\n\n\
[Install]\n\
WantedBy=multi-user.target\n",
    )?;
    fs::write(
        services_dir.join("worker.service"),
        b"[Unit]\n\
Description=Async Queue Worker\n\
After=network.target\n\n\
[Service]\n\
ExecStart=/usr/bin/python3 app/worker.py\n\
Restart=always\n\n\
[Install]\n\
WantedBy=multi-user.target\n",
    )?;

    // 7. Keys directory
    let keys_dir = workspace.join("keys");
    fs::create_dir_all(&keys_dir)?;
    let deploy_key = keys_dir.join("deploy_key.pem");
    fs::write(
        &deploy_key,
        b"-----BEGIN OPENSSH PRIVATE KEY-----\n\
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW\n\
QyNTUxOQAAACBg/x5qE1F/T8+fVdE8y9Y7P9xV9A1n4u8K0u57v19Q9wAAAECgB894oA\n\
fPeAAAAAtzc2gtZWQyNTUxOQAAACBg/x5qE1F/T8+fVdE8y9Y7P9xV9A1n4u8K0u57v1\n\
9Q9wAAAECgB894oAfPeAAAAAECAwQF\n\
-----END OPENSSH PRIVATE KEY-----\n",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&deploy_key, fs::Permissions::from_mode(0o644)).ok();
    }

    // Valid renewed certificate in keys/
    fs::write(
        keys_dir.join("new_cert.pem"),
        b"-----BEGIN CERTIFICATE-----\n\
MIIDITCCAomgAwIBAgIUValidRenewedCert20260911000000Z\n\
Issuer: CN=Acme Staging Root CA, O=FlightSim, C=US\n\
Validity: NotAfter: Dec 31 23:59:59 2028 GMT [STATUS: VALID]\n\
Subject: CN=gateway.payment.internal\n\
-----END CERTIFICATE-----\n",
    )?;

    // 8. Root README
    fs::write(
        workspace.join("README.md"),
        b"# Simulated Server Environment\n\n\
Welcome to the ShellPilot Linux Flight Simulator lab!\n\n\
Directories available to explore:\n\
- `app/`      : Python service, config, and HTML templates\n\
- `config/`   : Server, database, SSL certs, and environment config\n\
- `logs/`     : Web access logs, application error logs, auth logs\n\
- `data/`     : Customers CSV, inventory JSONL, and SQLite `production.db`\n\
- `scripts/`  : Executable maintenance and backup shell scripts\n\
- `services/` : Systemd service units and process definitions\n\
- `keys/`     : SSH deploy keys and renewed SSL certificates\n\n\
Powerful flight simulator commands:\n\
- `tour`      : Guided interactive overview of the entire simulator\n\
- `tree`      : Visual ASCII hierarchy of any directory\n\
- `service`   : Manage services: `service status|start|stop|restart|logs`\n\
- `curl`      : Test endpoints: `curl http://localhost:8080/health`\n\
- `db`        : Interactive SQL client: `db \"SELECT * FROM customers\"`\n\
- `ping`      : Test host latency: `ping localhost -c 3`\n\
- `whatif`    : Dry-run preview: `whatif 'rm -rf logs/*.log'`\n\
- `undo diff` : Preview changes before rolling back with `undo`\n\
- `cadet`     : View your flight rank, XP, and unlocked badges\n\
- `drill`     : Emergency production incident drills\n\
- `tutor`     : Full interactive Academy curriculum\n",
    )?;

    Ok(())
}

/// Execute a 'whatif' dry-run preview for a given command line.
pub fn execute_whatif(command_line: &str, current_dir: &Path) -> String {
    let ast = match parser::parse_ast(command_line, 0) {
        Ok(Some(ast)) => ast,
        Ok(None) => return "whatif: empty command".to_string(),
        Err(err) => return format!("whatif: syntax error: {err}"),
    };

    let mut out = String::new();
    out.push_str("🔍 [WHATIF: DRY-RUN PREVIEW]\n");
    out.push_str(&format!("Command: {command_line}\n\n"));

    let effects = effects::analyze_ast(&ast);
    let mut files_to_delete = Vec::new();
    let mut files_to_write = Vec::new();
    let mut sensitive_access = Vec::new();

    for effect in &effects {
        match effect {
            Effect::FilesystemDelete(path) => files_to_delete.push(path.clone()),
            Effect::FilesystemWrite(path) => files_to_write.push(path.clone()),
            Effect::SensitivePathWrite(path) => sensitive_access.push(format!("Write to sensitive path: {path}")),
            Effect::SensitivePathRead(path) => sensitive_access.push(format!("Read from sensitive path: {path}")),
            _ => {}
        }
    }

    // Inspect command arguments for deletion and wildcards
    let mut inspected_deletions = Vec::new();
    let mut total_delete_bytes: u64 = 0;

    walk_ast_commands(&ast, &mut |cmd| {
        if cmd.program == "rm" {
            for arg in &cmd.args {
                if arg.starts_with('-') {
                    continue;
                }
                let target_path = if Path::new(arg).is_absolute() {
                    PathBuf::from(arg)
                } else {
                    current_dir.join(arg)
                };

                // Check direct existence
                if target_path.exists() {
                    let size = fs::metadata(&target_path).map(|m| m.len()).unwrap_or(0);
                    total_delete_bytes += size;
                    inspected_deletions.push((arg.clone(), size));
                }
            }
        }
    });

    out.push_str("Actions:\n");
    if inspected_deletions.is_empty() && files_to_delete.is_empty() && files_to_write.is_empty() {
        out.push_str("  • No direct filesystem modifications detected.\n");
    }

    for (file, size) in &inspected_deletions {
        out.push_str(&format!(
            "  ❌ Delete: {} ({})\n",
            file,
            effects::format_bytes(*size)
        ));
    }

    for file in &files_to_write {
        let exists = current_dir.join(file).exists();
        let status = if exists { "Overwrite" } else { "Create" };
        out.push_str(&format!("  📝 {status}: {file}\n"));
    }

    for warning in &sensitive_access {
        out.push_str(&format!("  🚨 {warning}\n"));
    }

    out.push_str("\nSummary:\n");
    let del_count = inspected_deletions.len().max(files_to_delete.len());
    out.push_str(&format!("  • {del_count} file(s) target of deletion\n"));
    if total_delete_bytes > 0 {
        out.push_str(&format!(
            "  • Estimated space to be freed: {}\n",
            effects::format_bytes(total_delete_bytes)
        ));
    }
    out.push_str("⚠️  [Dry-run preview only. No actual changes were made to disk.]\n");

    out
}

fn walk_ast_commands<F>(ast: &Ast, callback: &mut F)
where
    F: FnMut(&crate::parser::ParsedCommand),
{
    match ast {
        Ast::Pipeline(p) => {
            for cmd in &p.commands {
                callback(cmd);
            }
        }
        Ast::Sequence(s) => {
            for item in &s.items {
                walk_ast_commands(&item.node, callback);
            }
        }
        Ast::If(if_stmt) => {
            for cmd in &if_stmt.condition.commands {
                callback(cmd);
            }
            for node in &if_stmt.then_branch {
                walk_ast_commands(node, callback);
            }
            if let Some(ref else_nodes) = if_stmt.else_branch {
                for node in else_nodes {
                    walk_ast_commands(node, callback);
                }
            }
        }
        Ast::For(for_loop) => {
            if let Ok(body) = for_loop.parse_body(0) {
                for node in &body {
                    walk_ast_commands(node, callback);
                }
            }
        }
        Ast::While(while_loop) => {
            if let Ok(condition) = while_loop.parse_condition(0) {
                for node in &condition {
                    walk_ast_commands(node, callback);
                }
            }
            if let Ok(body) = while_loop.parse_body(0) {
                for node in &body {
                    walk_ast_commands(node, callback);
                }
            }
        }
        Ast::Function(func) => {
            if let Ok(body) = func.parse_body(0) {
                for node in &body {
                    walk_ast_commands(node, callback);
                }
            }
        }
        Ast::StructuredPipeline(sp) => {
            walk_ast_commands(&sp.source, callback);
        }
    }
}

fn collect_file_paths(dir: &Path) -> io::Result<std::collections::HashSet<PathBuf>> {
    let mut set = std::collections::HashSet::new();
    if !dir.exists() {
        return Ok(set);
    }
    fn walk(base: &Path, curr: &Path, set: &mut std::collections::HashSet<PathBuf>) {
        if let Ok(entries) = fs::read_dir(curr) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(base, &path, set);
                } else if let Ok(rel) = path.strip_prefix(base) {
                    set.insert(rel.to_path_buf());
                }
            }
        }
    }
    walk(dir, dir, &mut set);
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_snapshot_and_undo_restores_files() {
        let temp = std::env::temp_dir().join(format!("shellpilot_test_undo_{}", std::process::id()));
        let mut sandbox = SandboxManager::with_root(temp).unwrap();

        // 1. Create file in workspace
        let file_path = sandbox.workspace.join("test.txt");
        fs::write(&file_path, "original content").unwrap();

        // 2. Snapshot state
        sandbox.pre_command_snapshot("rm test.txt").unwrap();

        // 3. Delete file
        fs::remove_file(&file_path).unwrap();
        assert!(!file_path.exists());

        // 4. Undo
        let msg = sandbox.undo().unwrap();
        assert!(msg.contains("Restored sandbox state"));
        assert!(file_path.exists());
        assert_eq!(fs::read_to_string(&file_path).unwrap(), "original content");

        sandbox.cleanup().ok();
    }

    #[test]
    fn path_jail_prevents_unauthorized_external_writes() {
        let temp = std::env::temp_dir().join(format!("shellpilot_test_jail_{}", std::process::id()));
        let sandbox = SandboxManager::with_root(temp).unwrap();

        // Inside workspace: allowed
        assert!(sandbox.is_path_jailed(Path::new("local.txt"), true));
        assert!(sandbox.is_path_jailed(&sandbox.workspace.join("file.txt"), true));

        // Outside workspace: forbidden for write
        assert!(!sandbox.is_path_jailed(Path::new("/etc/passwd"), true));
        assert!(!sandbox.is_path_jailed(Path::new("/home/user/file.txt"), true));

        // Path traversal with ../.. escaping sandbox: forbidden
        assert!(!sandbox.is_path_jailed(Path::new("../../../../../etc/passwd"), true));
    }

    #[test]
    fn whatif_previews_file_deletion() {
        let temp = std::env::temp_dir().join(format!("shellpilot_test_whatif_{}", std::process::id()));
        fs::create_dir_all(&temp).unwrap();
        let target = temp.join("victim.txt");
        fs::write(&target, "to be deleted").unwrap();

        let preview = execute_whatif("rm victim.txt", &temp);
        assert!(preview.contains("[WHATIF: DRY-RUN PREVIEW]"));
        assert!(preview.contains("Delete: victim.txt"));
        assert!(preview.contains("No actual changes were made to disk"));

        // Confirm file was NOT deleted
        assert!(target.exists());

        fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn diff_previous_snapshot_detects_changes() {
        let temp = std::env::temp_dir().join(format!("shellpilot_test_diff_{}", std::process::id()));
        let mut sandbox = SandboxManager::with_root(temp).unwrap();
        let file = sandbox.workspace.join("test.txt");
        fs::write(&file, "line1\nline2\n").unwrap();

        sandbox.pre_command_snapshot("modify test.txt").unwrap();

        fs::write(&file, "line1\nline_modified\n").unwrap();
        let diff = sandbox.diff_previous_snapshot().unwrap();
        assert!(diff.contains("TIME-TRAVEL DIFF"));
        assert!(diff.contains("Modified:"));
        assert!(diff.contains("test.txt"));

        sandbox.cleanup().ok();
    }
}
