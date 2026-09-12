use std::fs;
use std::path::Path;
use std::time::Instant;

pub struct Drill {
    pub id: &'static str,
    pub title: &'static str,
    pub severity: &'static str,
    pub scenario: &'static str,
    pub hint: &'static str,
    pub setup: fn(&Path) -> std::io::Result<()>,
    pub validate: fn(&Path) -> Result<String, String>,
}

pub struct DrillEngine {
    pub active_drill_id: Option<String>,
    pub started_at: Option<Instant>,
}

impl Default for DrillEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl DrillEngine {
    pub fn new() -> Self {
        Self {
            active_drill_id: None,
            started_at: None,
        }
    }

    pub fn list_drills(&self) -> String {
        let mut out = String::new();
        out.push_str("\n\x1b[1;36m╔══════════════════════════════════════════════════════════════════════════╗\x1b[0m\n");
        out.push_str("║                 🚨  EMERGENCY INCIDENT DRILLS & CHAOS ROOM               ║\n");
        out.push_str("\x1b[1;36m╚══════════════════════════════════════════════════════════════════════════╝\x1b[0m\n\n");
        out.push_str("Real-world production outages to test your troubleshooting under pressure:\n\n");

        for drill in ALL_DRILLS {
            let status = if self.active_drill_id.as_deref() == Some(drill.id) {
                "\x1b[1;33m▶ ACTIVE INCIDENT\x1b[0m"
            } else {
                "\x1b[0;90m○ READY TO TRIGGER\x1b[0m"
            };

            let sev_color = match drill.severity {
                "P1 - CRITICAL" => "\x1b[1;31m",
                "P2 - HIGH" => "\x1b[1;33m",
                _ => "\x1b[1;34m",
            };

            out.push_str(&format!(
                "  {:<26} {}{:<14}\x1b[0m \x1b[1m{}\x1b[0m ({})\n     \x1b[0;90m└─ {}\x1b[0m\n\n",
                status, sev_color, drill.severity, drill.title, drill.id, drill.scenario
            ));
        }

        out.push_str("💡 Commands:\n");
        out.push_str("   drill start <drill_id>  Trigger a scenario (e.g. 'drill start drill-disk')\n");
        out.push_str("   drill check             Verify whether you resolved the incident\n");
        out.push_str("   drill hint              Get a clue if troubleshooting gets stalled\n\n");
        out
    }

    pub fn start_drill(&mut self, id: &str, workspace: &Path) -> Result<String, String> {
        let drill = ALL_DRILLS
            .iter()
            .find(|d| d.id == id)
            .ok_or_else(|| format!("drill: unknown drill ID '{}'. Type 'drill' to list available.", id))?;

        (drill.setup)(workspace).map_err(|e| format!("drill setup failed: {e}"))?;

        self.active_drill_id = Some(id.to_string());
        self.started_at = Some(Instant::now());

        let mut out = String::new();
        out.push_str("\n\x1b[1;31m╔══════════════════════════════════════════════════════════════════════════╗\x1b[0m\n");
        out.push_str(&format!("\x1b[1;31m║  🚨 INCIDENT ALERT [{:<13}] {:<34} ║\x1b[0m\n", drill.severity, drill.title));
        out.push_str("\x1b[1;31m╚══════════════════════════════════════════════════════════════════════════╝\x1b[0m\n\n");
        out.push_str(&format!("📖 \x1b[1mIncident Briefing:\x1b[0m\n   {}\n\n", drill.scenario));
        out.push_str("🎯 \x1b[1mCadet Mission:\x1b[0m\n   Use standard Linux inspection tools to locate the problem, repair it, and run '\x1b[1;33mdrill check\x1b[0m'.\n");
        out.push_str("⏱️  Incident response timer started!\n\n");

        Ok(out)
    }

    pub fn check_active(&mut self, workspace: &Path) -> Result<String, String> {
        let id = self
            .active_drill_id
            .as_ref()
            .ok_or_else(|| "drill: No active drill running. Trigger one with 'drill start <id>'.".to_string())?;

        let drill = ALL_DRILLS
            .iter()
            .find(|d| d.id == id)
            .ok_or_else(|| "drill not found".to_string())?;

        let elapsed = self.started_at.map(|t| t.elapsed().as_secs()).unwrap_or(0);

        match (drill.validate)(workspace) {
            Ok(msg) => {
                self.active_drill_id = None;
                self.started_at = None;

                let mut out = String::new();
                out.push_str("\n\x1b[1;32m╔══════════════════════════════════════════════════════════════════════════╗\x1b[0m\n");
                out.push_str("║                   🏆  INCIDENT RESOLVED! GOOD JOB!                      ║\n");
                out.push_str("\x1b[1;32m╚══════════════════════════════════════════════════════════════════════════╝\x1b[0m\n\n");
                out.push_str(&format!("🎉 {}\n", msg));
                out.push_str(&format!("⏱️  Resolution Time : {} seconds\n", elapsed));
                out.push_str("⭐ \x1b[1;33m+100 XP awarded to your Cadet Dossier!\x1b[0m\n\n");
                Ok(out)
            }
            Err(e) => Err(format!("⏳ [INCIDENT PERSISTS]: {}\nKeep investigating or type 'drill hint'.", e)),
        }
    }

    pub fn hint_active(&self) -> Option<String> {
        let id = self.active_drill_id.as_ref()?;
        let drill = ALL_DRILLS.iter().find(|d| d.id == id)?;
        Some(format!("💡 Drill Hint:\n   {}\n", drill.hint))
    }
}

pub const ALL_DRILLS: &[Drill] = &[
    Drill {
        id: "drill-disk",
        title: "Runaway Crash Dump Quota",
        severity: "P1 - CRITICAL",
        scenario: "The monitoring alert fired: simulated disk quota is exhausted by a rogue crash dump in 'logs/'. Find and remove the large crash dump without deleting live logs.",
        hint: "1. Inspect 'logs/' with 'ls -lh logs/' to find the large crash dump.\n   2. Remove or truncate 'logs/crash_dump.tmp' using 'rm logs/crash_dump.tmp' (or ': > logs/crash_dump.tmp').\n   3. Ensure live logs like 'logs/access.log' remain untouched.",
        setup: |ws| {
            let p = ws.join("logs/crash_dump.tmp");
            fs::write(p, "DUMP_GARBAGE\n".repeat(5000))
        },
        validate: |ws| {
            let p = ws.join("logs/crash_dump.tmp");
            if p.exists() && fs::metadata(&p).map(|m| m.len() > 0).unwrap_or(true) {
                return Err("'logs/crash_dump.tmp' is still present with non-zero size on disk.".to_string());
            }
            if !ws.join("logs/access.log").exists() {
                return Err("Critical error: live 'logs/access.log' was accidentally deleted!".to_string());
            }
            Ok("Crash dump safely purged! Live log integrity verified.".to_string())
        },
    },
    Drill {
        id: "drill-service",
        title: "Payment API Crash Loop",
        severity: "P1 - CRITICAL",
        scenario: "Payment web service crashed on startup and refuses to boot. Check 'service logs web', repair corrupted port in 'config/server.conf' back to 8080, and restart with 'service start web'.",
        hint: "1. Inspect failure logs with 'service logs web' or view 'config/server.conf'.\n   2. Fix the corrupted port line: replace 'NaN_PORT_CRASH' with '8080' (e.g. sed -i 's/NaN_PORT_CRASH/8080/' config/server.conf).\n   3. Restart the service with 'service start web'.",
        setup: |ws| {
            let p = ws.join("config/server.conf");
            if p.exists() {
                let text = fs::read_to_string(&p)?;
                let broken = text.replace("8080", "NaN_PORT_CRASH");
                fs::write(p, broken)?;
            }
            Ok(())
        },
        validate: |ws| {
            let p = ws.join("config/server.conf");
            if let Ok(text) = fs::read_to_string(&p) {
                if text.contains("NaN_PORT_CRASH") {
                    return Err("'config/server.conf' still contains corrupted 'NaN_PORT_CRASH'.".to_string());
                }
                if text.contains("port = 8080") || text.contains("port=8080") {
                    return Ok("Payment web service configuration restored and verified!".to_string());
                }
            }
            Err("Port is not set to 8080 in 'config/server.conf'.".to_string())
        },
    },
    Drill {
        id: "drill-lock",
        title: "Deadlocked Worker Lockfile",
        severity: "P2 - HIGH",
        scenario: "The transaction queue worker cannot boot because a crashed previous run left an uncleaned PID lockfile at 'services/worker.lock'. Remove the lockfile and start the worker.",
        hint: "1. Check the services folder with 'ls -la services/'.\n   2. A stale PID lockfile blocks boot. Remove it with 'rm services/worker.lock'.\n   3. Start the queue worker with 'service start worker' or verify with 'drill check'.",
        setup: |ws| {
            fs::create_dir_all(ws.join("services"))?;
            fs::write(ws.join("services/worker.lock"), "19842\n")
        },
        validate: |ws| {
            let p = ws.join("services/worker.lock");
            if p.exists() {
                return Err("Lockfile 'services/worker.lock' still exists.".to_string());
            }
            Ok("Stale lockfile cleared! Transaction queue unblocked.".to_string())
        },
    },
    Drill {
        id: "drill-perm",
        title: "Security Breach: Key Permissions",
        severity: "P1 - CRITICAL",
        scenario: "Security scanner detected that SSH private key 'keys/deploy_key.pem' is world-writable (mode 777)! Immediately restrict it to owner-only mode 600.",
        hint: "1. Check current key permissions with 'ls -l keys/deploy_key.pem'.\n   2. SSH keys must be strictly owner-only (mode 600) to prevent unauthorized access.\n   3. Run 'chmod 600 keys/deploy_key.pem', then verify with 'drill check'.",
        setup: |ws| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let p = ws.join("keys/deploy_key.pem");
                if p.exists() {
                    let _ = fs::set_permissions(&p, fs::Permissions::from_mode(0o777));
                }
            }
            Ok(())
        },
        validate: |ws| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let p = ws.join("keys/deploy_key.pem");
                if let Ok(meta) = fs::metadata(&p) {
                    let mode = meta.permissions().mode() & 0o777;
                    if mode == 0o600 {
                        return Ok("SSH private key hardened to owner-only mode 600!".to_string());
                    } else {
                        return Err(format!("Current mode is {:o}, expected 600.", mode));
                    }
                }
            }
            Ok("Validated permissions.".to_string())
        },
    },
    Drill {
        id: "drill-cert",
        title: "Expired SSL Certificate Emergency",
        severity: "P1 - CRITICAL",
        scenario: "The payment gateway is throwing SSL handshake failures because 'config/ssl/cert.pem' has expired. A valid renewed certificate is staged in 'keys/new_cert.pem'. Copy 'keys/new_cert.pem' into 'config/ssl/cert.pem' to restore gateway encryption.",
        hint: "1. Compare certificates: 'cat config/ssl/cert.pem' (expired) and 'cat keys/new_cert.pem' (renewed).\n   2. Deploy the renewed certificate: 'cp keys/new_cert.pem config/ssl/cert.pem'.\n   3. Run 'drill check' to verify SSL gateway health.",
        setup: |ws| {
            let ssl_dir = ws.join("config/ssl");
            fs::create_dir_all(&ssl_dir)?;
            fs::write(
                ssl_dir.join("cert.pem"),
                b"-----BEGIN CERTIFICATE-----\nMIIDITCCAomgAwIBAgIUStaleExpiredCert20250911000000Z\nStatus: EXPIRED\n-----END CERTIFICATE-----\n",
            )?;
            let keys_dir = ws.join("keys");
            fs::create_dir_all(&keys_dir)?;
            fs::write(
                keys_dir.join("new_cert.pem"),
                b"-----BEGIN CERTIFICATE-----\nMIIDITCCAomgAwIBAgIUValidRenewedCert20260911000000Z\nStatus: VALID\n-----END CERTIFICATE-----\n",
            )?;
            Ok(())
        },
        validate: |ws| {
            let cert_path = ws.join("config/ssl/cert.pem");
            if let Ok(content) = fs::read_to_string(&cert_path) {
                if content.contains("VALID") || content.contains("ValidRenewedCert") {
                    return Ok("Renewed SSL certificate successfully deployed!".to_string());
                } else if content.contains("EXPIRED") {
                    return Err("'config/ssl/cert.pem' is still the expired certificate.".to_string());
                }
            }
            Err("Valid certificate not found in 'config/ssl/cert.pem'.".to_string())
        },
    },
    Drill {
        id: "drill-dos",
        title: "Denial-of-Service Flood Defense",
        severity: "P1 - CRITICAL",
        scenario: "Monitoring alerts report a high-volume request flood in 'logs/access.log'. Extract client IPs to identify the abusive address, and append it to 'config/blocklist.conf' to block the attack.",
        hint: "1. Analyze web traffic in 'logs/access.log': 'cut -d\" \" -f1 logs/access.log | sort | uniq -c | sort -n'.\n   2. The abusive flood originates from IP '198.51.100.42'.\n   3. Append it to the blocklist: 'echo 198.51.100.42 >> config/blocklist.conf'.",
        setup: |ws| {
            let logs_dir = ws.join("logs");
            fs::create_dir_all(&logs_dir)?;
            let mut flood = String::new();
            for _ in 0..150 {
                flood.push_str("198.51.100.42 - - [11/Sep/2026:10:15:00 +0000] \"GET /api/v1/payment HTTP/1.1\" 503 120\n");
            }
            fs::write(logs_dir.join("access.log"), flood)?;
            fs::create_dir_all(ws.join("config"))?;
            fs::write(ws.join("config/blocklist.conf"), "# IP Blocklist\n")?;
            Ok(())
        },
        validate: |ws| {
            let blocklist = ws.join("config/blocklist.conf");
            let alt_blocklist = ws.join("blocklist.conf");
            let found = fs::read_to_string(&blocklist).map(|c| c.contains("198.51.100.42")).unwrap_or(false)
                || fs::read_to_string(&alt_blocklist).map(|c| c.contains("198.51.100.42")).unwrap_or(false);
            if found {
                return Ok("Attacking IP 198.51.100.42 successfully neutralized in blocklist!".to_string());
            }
            Err("Offending IP 198.51.100.42 not found in 'config/blocklist.conf'.".to_string())
        },
    },
    Drill {
        id: "drill-db",
        title: "Database Host Desynchronization",
        severity: "P2 - HIGH",
        scenario: "The service in 'app/config.json' cannot reach its backend because 'database.host' is configured to 'dead-db-server.internal'. Change it to '127.0.0.1' or 'localhost' to re-establish connectivity.",
        hint: "1. Inspect database configuration in 'app/config.json'.\n   2. Update 'dead-db-server.internal' to active local endpoint '127.0.0.1' or 'localhost' (e.g. sed -i 's/dead-db-server.internal/127.0.0.1/' app/config.json).\n   3. Run 'drill check' to verify database connectivity.",
        setup: |ws| {
            let app_dir = ws.join("app");
            fs::create_dir_all(&app_dir)?;
            let cfg = ws.join("app/config.json");
            let text = "{\n  \"appName\": \"PaymentGateway\",\n  \"database\": {\n    \"host\": \"dead-db-server.internal\",\n    \"port\": 5432\n  }\n}\n";
            fs::write(cfg, text)?;
            Ok(())
        },
        validate: |ws| {
            let cfg = ws.join("app/config.json");
            if let Ok(content) = fs::read_to_string(&cfg) {
                if content.contains("dead-db-server.internal") {
                    return Err("'app/config.json' still contains disconnected host 'dead-db-server.internal'.".to_string());
                }
                if content.contains("127.0.0.1") || content.contains("localhost") {
                    return Ok("Database host synchronized to active local endpoint!".to_string());
                }
            }
            Err("Database host not set to 127.0.0.1 or localhost in 'app/config.json'.".to_string())
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drill_disk_lifecycle() {
        let path = std::env::temp_dir().join(format!("shellpilot_test_drill_{}", std::process::id()));
        fs::create_dir_all(path.join("logs")).unwrap();
        fs::write(path.join("logs/access.log"), "live log\n").unwrap();

        let mut engine = DrillEngine::new();
        let start_msg = engine.start_drill("drill-disk", &path).unwrap();
        assert!(start_msg.contains("INCIDENT ALERT"));
        assert!(path.join("logs/crash_dump.tmp").exists());

        // Check fails before fixing
        assert!(engine.check_active(&path).is_err());

        // Fix by removing crash dump
        fs::remove_file(path.join("logs/crash_dump.tmp")).unwrap();

        // Check succeeds
        let check_res = engine.check_active(&path).unwrap();
        assert!(check_res.contains("INCIDENT RESOLVED"));

        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn drill_cert_and_dos_lifecycle() {
        let path = std::env::temp_dir().join(format!("shellpilot_test_drill2_{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();

        let mut engine = DrillEngine::new();

        // Cert drill
        engine.start_drill("drill-cert", &path).unwrap();
        assert!(engine.check_active(&path).is_err());
        fs::copy(path.join("keys/new_cert.pem"), path.join("config/ssl/cert.pem")).unwrap();
        assert!(engine.check_active(&path).is_ok());

        // DoS drill
        engine.start_drill("drill-dos", &path).unwrap();
        assert!(engine.check_active(&path).is_err());
        fs::write(path.join("config/blocklist.conf"), "198.51.100.42\n").unwrap();
        assert!(engine.check_active(&path).is_ok());

        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn test_all_7_drills_lifecycle_and_validation() {
        let path = std::env::temp_dir().join(format!("shellpilot_test_all_drills_{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();

        let mut engine = DrillEngine::new();

        for drill in ALL_DRILLS {
            // Populate basic sandbox structure if not present
            fs::create_dir_all(path.join("logs")).unwrap();
            fs::write(path.join("logs/access.log"), "live logs\n").unwrap();
            fs::create_dir_all(path.join("config")).unwrap();
            fs::create_dir_all(path.join("services")).unwrap();
            fs::create_dir_all(path.join("keys")).unwrap();
            fs::create_dir_all(path.join("app")).unwrap();
            fs::write(path.join("keys/deploy_key.pem"), "key data\n").unwrap();

            engine.start_drill(drill.id, &path).unwrap();
            assert!(engine.check_active(&path).is_err(), "Drill {} should be failing before fix", drill.id);

            match drill.id {
                "drill-disk" => {
                    let dump = path.join("logs/crash_dump.tmp");
                    if dump.exists() {
                        fs::remove_file(dump).unwrap();
                    }
                }
                "drill-service" => {
                    fs::write(path.join("config/server.conf"), "port = 8080\n").unwrap();
                }
                "drill-lock" => {
                    let lock = path.join("services/worker.lock");
                    if lock.exists() {
                        fs::remove_file(lock).unwrap();
                    }
                }
                "drill-perm" => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(path.join("keys/deploy_key.pem"), fs::Permissions::from_mode(0o600)).unwrap();
                    }
                }
                "drill-cert" => {
                    fs::copy(path.join("keys/new_cert.pem"), path.join("config/ssl/cert.pem")).unwrap();
                }
                "drill-dos" => {
                    fs::write(path.join("config/blocklist.conf"), "198.51.100.42\n").unwrap();
                }
                "drill-db" => {
                    fs::write(path.join("app/config.json"), "{\"database\": {\"host\": \"127.0.0.1\"}}\n").unwrap();
                }
                _ => panic!("Unhandled drill in test: {}", drill.id),
            }

            let check_res = engine.check_active(&path);
            assert!(check_res.is_ok(), "Drill {} failed check after repair: {:?}", drill.id, check_res);
        }

        fs::remove_dir_all(&path).ok();
    }
}
