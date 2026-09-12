use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq)]
pub enum ServiceState {
    Stopped,
    Running { pid: u32, started_at: u64 },
    Failed { reason: String },
}

#[derive(Clone, Debug)]
pub struct MockService {
    pub name: &'static str,
    pub description: &'static str,
    pub port: Option<u16>,
    pub state: ServiceState,
}

pub struct ServerSimulator {
    services: RefCell<Vec<MockService>>,
}

impl Default for ServerSimulator {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerSimulator {
    pub fn new() -> Self {
        Self {
            services: RefCell::new(vec![
                MockService {
                    name: "web",
                    description: "Payment Gateway HTTP Web Service",
                    port: Some(8080),
                    state: ServiceState::Stopped,
                },
                MockService {
                    name: "worker",
                    description: "Payment Transaction Queue Worker",
                    port: None,
                    state: ServiceState::Stopped,
                },
            ]),
        }
    }

    pub fn is_web_running(&self) -> bool {
        self.services
            .borrow()
            .iter()
            .any(|s| s.name == "web" && matches!(s.state, ServiceState::Running { .. }))
    }

    pub fn is_web_running_in(&self, workspace: &Path) -> bool {
        self.sync_with_workspace(workspace);
        self.is_web_running()
    }

    pub fn sync_with_workspace(&self, workspace: &Path) {
        let mut services = self.services.borrow_mut();
        for svc in services.iter_mut() {
            let pid_path = workspace.join("services").join(format!(".{}.pid", svc.name));
            if pid_path.exists() {
                if let Ok(content) = fs::read_to_string(&pid_path) {
                    let mut lines = content.lines();
                    let pid = lines.next().and_then(|l| l.parse::<u32>().ok()).unwrap_or(2410);
                    let started_at = lines.next().and_then(|l| l.parse::<u64>().ok()).unwrap_or(0);
                    svc.state = ServiceState::Running { pid, started_at };
                }
            } else if matches!(svc.state, ServiceState::Running { .. }) {
                svc.state = ServiceState::Stopped;
            }
        }
    }

    pub fn handle_service_command(&self, args: &[String], workspace: &Path) -> Result<String, String> {
        self.sync_with_workspace(workspace);
        if args.is_empty() || args[0] == "status" {
            let target = args.get(1).map(|s| s.as_str());
            return Ok(self.status_overview(target));
        }

        let action = args[0].as_str();
        let name = match args.get(1) {
            Some(n) => n.as_str(),
            None => {
                return Err(format!(
                    "service: action '{}' requires a service name (e.g. 'service {} web')",
                    action, action
                ));
            }
        };

        match action {
            "start" => self.start_service(name, workspace),
            "stop" => self.stop_service(name, workspace),
            "restart" => {
                self.stop_service(name, workspace)?;
                self.start_service(name, workspace)
            }
            "logs" => self.get_service_logs(name),
            _ => Err(format!(
                "service: unknown action '{}'. Available: status, start, stop, restart, logs",
                action
            )),
        }
    }

    fn start_service(&self, name: &str, workspace: &Path) -> Result<String, String> {
        let mut services = self.services.borrow_mut();
        let svc = services
            .iter_mut()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("service: unknown service '{}'. Available: web, worker", name))?;

        if matches!(svc.state, ServiceState::Running { .. }) {
            return Ok(format!("service '{}' is already running.", name));
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        if name == "web" {
            // Validate config/server.conf
            let conf_path = workspace.join("config/server.conf");
            if conf_path.exists()
                && let Ok(content) = fs::read_to_string(&conf_path)
                    && content.contains("NaN_PORT_CRASH") {
                        svc.state = ServiceState::Failed {
                            reason: "Port parsing error: invalid integer 'NaN_PORT_CRASH' in config/server.conf:4".to_string(),
                        };
                        let _ = fs::remove_file(workspace.join("services/.web.pid"));
                        return Err("❌ Failed to start web.service: Configuration syntax error in config/server.conf.\n   Check 'service logs web' for details.".to_string());
                    }

            svc.state = ServiceState::Running {
                pid: 2410,
                started_at: now,
            };
            let _ = fs::create_dir_all(workspace.join("services"));
            let _ = fs::write(workspace.join("services/.web.pid"), format!("2410\n{}", now));
            Ok("🚀 Started web.service: Payment Gateway HTTP Web Service [PID 2410, Port 8080]".to_string())
        } else if name == "worker" {
            // Check for stale lockfile
            let lock_path = workspace.join("services/worker.lock");
            if lock_path.exists() {
                svc.state = ServiceState::Failed {
                    reason: "Stale lockfile detected at services/worker.lock (PID 19842 is dead)".to_string(),
                };
                let _ = fs::remove_file(workspace.join("services/.worker.pid"));
                return Err("❌ Failed to start worker.service: Lockfile exists at services/worker.lock.\n   Remove stale lockfile and run 'service start worker'.".to_string());
            }

            svc.state = ServiceState::Running {
                pid: 2411,
                started_at: now,
            };
            let _ = fs::create_dir_all(workspace.join("services"));
            let _ = fs::write(workspace.join("services/.worker.pid"), format!("2411\n{}", now));
            Ok("🚀 Started worker.service: Payment Transaction Queue Worker [PID 2411]".to_string())
        } else {
            Err(format!("service: unknown service '{}'", name))
        }
    }

    fn stop_service(&self, name: &str, workspace: &Path) -> Result<String, String> {
        let mut services = self.services.borrow_mut();
        let svc = services
            .iter_mut()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("service: unknown service '{}'", name))?;

        let _ = fs::remove_file(workspace.join("services").join(format!(".{name}.pid")));
        match svc.state {
            ServiceState::Running { pid, .. } => {
                svc.state = ServiceState::Stopped;
                Ok(format!("🛑 Stopped {}.service [PID {}]", name, pid))
            }
            ServiceState::Failed { .. } => {
                svc.state = ServiceState::Stopped;
                Ok(format!("Cleared failed state for {}.service.", name))
            }
            ServiceState::Stopped => Ok(format!("service '{}' was already stopped.", name)),
        }
    }

    fn get_service_logs(&self, name: &str) -> Result<String, String> {
        let services = self.services.borrow();
        let svc = services
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("service: unknown service '{}'", name))?;

        let mut out = String::new();
        out.push_str(&format!("\x1b[1;36m📑 Logs for {}.service:\x1b[0m\n", name));

        match &svc.state {
            ServiceState::Running { pid, .. } => {
                out.push_str(&format!("  [systemd] Started {}.service.\n", name));
                if name == "web" {
                    out.push_str(&format!("  [payment-web:{}] Listening on http://0.0.0.0:8080 (Press Ctrl+C to stop)\n", pid));
                    out.push_str(&format!("  [payment-web:{}] Connected to PostgreSQL pool at 10.0.4.12:5432\n", pid));
                    out.push_str(&format!("  [payment-web:{}] Healthcheck endpoint ready at /health\n", pid));
                } else {
                    out.push_str(&format!("  [payment-worker:{}] Connected to Redis queue 'tx_pending'\n", pid));
                    out.push_str(&format!("  [payment-worker:{}] Processed 148 transactions, 0 errors\n", pid));
                }
            }
            ServiceState::Failed { reason } => {
                out.push_str(&format!("  \x1b[1;31m[CRASH]\x1b[0m Failed to start {}.service\n", name));
                out.push_str(&format!("  \x1b[1;31m[FATAL]\x1b[0m {}\n", reason));
                out.push_str("  [systemd] Unit entered failed state.\n");
            }
            ServiceState::Stopped => {
                out.push_str(&format!("  [systemd] {}.service is currently stopped.\n", name));
            }
        }
        Ok(out)
    }

    fn status_overview(&self, target: Option<&str>) -> String {
        let services = self.services.borrow();
        let mut out = String::new();

        if let Some(name) = target {
            if let Some(svc) = services.iter().find(|s| s.name == name) {
                out.push_str(&format!("\x1b[1m● {}.service\x1b[0m - {}\n", svc.name, svc.description));
                match &svc.state {
                    ServiceState::Running { pid, started_at } => {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        let uptime = now.saturating_sub(*started_at);
                        out.push_str("   Loaded: loaded (services/web.service)\n");
                        out.push_str(&format!(
                            "   Active: \x1b[1;32mactive (running)\x1b[0m since {}s ago\n",
                            uptime
                        ));
                        out.push_str(&format!(" Main PID: {} ({})\n", pid, svc.name));
                        if let Some(port) = svc.port {
                            out.push_str(&format!("     Port: {}\n", port));
                        }
                    }
                    ServiceState::Failed { reason } => {
                        out.push_str("   Loaded: loaded (services/web.service)\n");
                        out.push_str("   Active: \x1b[1;31mfailed (Result: exit-code)\x1b[0m\n");
                        out.push_str(&format!("    Error: {}\n", reason));
                    }
                    ServiceState::Stopped => {
                        out.push_str("   Loaded: loaded (services/web.service)\n");
                        out.push_str("   Active: \x1b[0;90minactive (dead)\x1b[0m\n");
                    }
                }
                return out;
            } else {
                return format!("service: unknown service '{}'\n", name);
            }
        }

        out.push_str("\n\x1b[1;36m╔══════════════════════════════════════════════════════════════════════╗\x1b[0m\n");
        out.push_str("║               ⚙️   SHELLPILOT MOCK SERVICE MANAGER                    ║\x1b[0m\n");
        out.push_str("\x1b[1;36m╚══════════════════════════════════════════════════════════════════════╝\x1b[0m\n\n");

        for svc in services.iter() {
            let (status_badge, pid_str) = match &svc.state {
                ServiceState::Running { pid, .. } => ("\x1b[1;32m● RUNNING\x1b[0m", format!("PID {}", pid)),
                ServiceState::Failed { .. } => ("\x1b[1;31m● FAILED \x1b[0m", "CRASHED".to_string()),
                ServiceState::Stopped => ("\x1b[0;90m○ STOPPED\x1b[0m", "-".to_string()),
            };

            let port_str = svc
                .port
                .map(|p| format!(":{}", p))
                .unwrap_or_else(|| "     ".to_string());

            out.push_str(&format!(
                "  {:<18} {:<10} {:<10} {:<6} {}\n",
                status_badge, svc.name, pid_str, port_str, svc.description
            ));
        }

        out.push_str("\n\x1b[0;90m💡 Commands: 'service start <name>', 'service stop <name>', 'service logs <name>'\x1b[0m\n\n");
        out
    }

    pub fn handle_curl(&self, args: &[String], workspace: &Path) -> Result<String, String> {
        self.sync_with_workspace(workspace);
        let mut head_only = false;
        let mut url_opt = None;

        for arg in args {
            match arg.as_str() {
                "-I" | "--head" => head_only = true,
                "-s" | "--silent" => {}
                u if u.starts_with("http://") || u.starts_with("https://") || u.starts_with("localhost") => {
                    url_opt = Some(u.to_string());
                }
                _ => {}
            }
        }

        let url = match url_opt {
            Some(u) => u,
            None => {
                return Err("curl: try 'curl http://localhost:8080/health' or 'curl http://localhost:8080/api/customers'".to_string());
            }
        };

        if !url.contains("localhost:8080") && !url.contains("127.0.0.1:8080") {
            return Err(format!(
                "curl: (7) Mock simulator currently emulates 'http://localhost:8080' (web.service).\n       Connection refused for target '{}'",
                url
            ));
        }

        if !self.is_web_running() {
            return Err("curl: (7) Failed to connect to localhost port 8080: Connection refused\n💡 Tip: The web service is stopped. Run 'service start web' first.".to_string());
        }

        let path = if let Some(idx) = url.find(":8080") {
            &url[idx + 5..]
        } else {
            "/"
        };

        let clean_path = path.trim_end_matches('/');

        match clean_path {
            "" | "/health" => {
                if head_only {
                    Ok("HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nServer: shellpilot-mock/2.4.1\r\nConnection: keep-alive\r\n\r\n".to_string())
                } else {
                    Ok(r#"{
  "status": "UP",
  "service": "payment-web",
  "version": "2.4.1",
  "checks": {
    "database": "UP",
    "cache": "UP"
  }
}"#.to_string())
                }
            }
            "/api/status" => {
                if head_only {
                    Ok("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n".to_string())
                } else {
                    Ok(r#"{
  "active_workers": 4,
  "queue_depth": 0,
  "db_pool": {
    "active": 2,
    "idle": 8,
    "max": 10
  }
}"#.to_string())
                }
            }
            "/api/customers" => {
                let csv_path = workspace.join("data/customers.csv");
                if let Ok(csv) = fs::read_to_string(&csv_path) {
                    let mut lines = csv.lines();
                    let _header = lines.next();
                    let mut items = Vec::new();

                    for line in lines {
                        let parts: Vec<&str> = line.split(',').collect();
                        if parts.len() >= 5 {
                            items.push(format!(
                                r#"    {{"id": "{}", "name": "{}", "email": "{}", "plan": "{}", "status": "{}"}}"#,
                                parts[0], parts[1], parts[2], parts[3], parts[4]
                            ));
                        }
                    }

                    let json = format!("[\n{}\n]", items.join(",\n"));
                    if head_only {
                        Ok(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", json.len()))
                    } else {
                        Ok(json)
                    }
                } else {
                    Ok("[]".to_string())
                }
            }
            _ => {
                if head_only {
                    Ok("HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\n\r\n".to_string())
                } else {
                    Ok("404 Not Found: Endpoint does not exist. Available: /health, /api/status, /api/customers".to_string())
                }
            }
        }
    }
}

pub fn ensure_production_db(db_path: &Path) -> Result<rusqlite::Connection, String> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("failed to create db directory: {e}"))?;
    }
    let conn = rusqlite::Connection::open(db_path).map_err(|e| format!("failed to open sqlite database: {e}"))?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS customers (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            email TEXT NOT NULL,
            plan TEXT NOT NULL,
            status TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS orders (
            id INTEGER PRIMARY KEY,
            customer_id INTEGER NOT NULL,
            amount REAL NOT NULL,
            currency TEXT NOT NULL,
            status TEXT NOT NULL,
            FOREIGN KEY (customer_id) REFERENCES customers(id)
        );
        CREATE TABLE IF NOT EXISTS system_metrics (
            id INTEGER PRIMARY KEY,
            metric TEXT NOT NULL,
            value REAL NOT NULL,
            recorded_at TEXT NOT NULL
        );"
    ).map_err(|e| format!("failed to initialize db schema: {e}"))?;

    let count: i64 = conn.query_row("SELECT COUNT(*) FROM customers", [], |r| r.get(0)).unwrap_or(0);
    if count == 0 {
        conn.execute_batch(
            "INSERT INTO customers (id, name, email, plan, status) VALUES
                (1001, 'Alice Chen', 'alice@acme.corp', 'enterprise', 'active'),
                (1002, 'Bob Martinez', 'bob@cyberdyne.io', 'pro', 'active'),
                (1003, 'Charlie Kim', 'charlie@starlight.dev', 'starter', 'active'),
                (1004, 'Dana Scully', 'dana@fbi.gov', 'enterprise', 'suspended'),
                (1005, 'Evan Wright', 'evan@quantum.net', 'pro', 'active');

            INSERT INTO orders (id, customer_id, amount, currency, status) VALUES
                (5001, 1001, 2400.00, 'USD', 'completed'),
                (5002, 1001, 1200.00, 'USD', 'completed'),
                (5003, 1002, 450.00, 'USD', 'completed'),
                (5004, 1003, 99.00, 'USD', 'pending'),
                (5005, 1004, 4800.00, 'USD', 'failed'),
                (5006, 1005, 750.00, 'USD', 'completed');

            INSERT INTO system_metrics (metric, value, recorded_at) VALUES
                ('cpu_usage_pct', 24.5, '2026-09-11 12:00:00'),
                ('memory_used_mb', 1420.0, '2026-09-11 12:00:00'),
                ('active_connections', 42.0, '2026-09-11 12:00:00'),
                ('http_requests_per_sec', 185.2, '2026-09-11 12:00:00');"
        ).map_err(|e| format!("failed to seed database: {e}"))?;
    }

    Ok(conn)
}

pub fn run_db_command(workspace: &Path, args: &[String]) -> Result<String, String> {
    let db_path = workspace.join("data/production.db");
    let conn = ensure_production_db(&db_path)?;

    if args.is_empty() || args[0] == "help" || args[0] == "-h" || args[0] == "--help" {
        let cust_count: i64 = conn.query_row("SELECT COUNT(*) FROM customers", [], |r| r.get(0)).unwrap_or(0);
        let ord_count: i64 = conn.query_row("SELECT COUNT(*) FROM orders", [], |r| r.get(0)).unwrap_or(0);
        let met_count: i64 = conn.query_row("SELECT COUNT(*) FROM system_metrics", [], |r| r.get(0)).unwrap_or(0);

        let mut out = String::new();
        out.push_str("\n🗄️  SHELLPILOT EMBEDDED SQL DATABASE (data/production.db)\n");
        out.push_str("═══════════════════════════════════════════════════════════════════════\n");
        out.push_str("Available Tables:\n");
        out.push_str(&format!("  • customers      ({cust_count} rows)  - Customer accounts and subscription tiers\n"));
        out.push_str(&format!("  • orders         ({ord_count} rows)  - Financial transaction amounts and statuses\n"));
        out.push_str(&format!("  • system_metrics ({met_count} rows)  - Performance and resource metrics\n\n"));
        out.push_str("Usage:\n");
        out.push_str("  db schema                 Show CREATE TABLE statements\n");
        out.push_str("  db tables                 List all table names\n");
        out.push_str("  db \"<SQL query>\"          Execute query and format results\n\n");
        out.push_str("Examples:\n");
        out.push_str("  db \"SELECT * FROM customers WHERE plan = 'enterprise'\"\n");
        out.push_str("  db \"SELECT c.name, o.amount, o.status FROM customers c JOIN orders o ON c.id = o.customer_id\"\n");
        return Ok(out);
    }

    if args[0] == "tables" {
        let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
        let mut names = Vec::new();
        for r in rows.flatten() {
            names.push(r);
        }
        return Ok(format!("Tables:\n{}", names.iter().map(|n| format!("  • {n}")).collect::<Vec<_>>().join("\n")));
    }

    if args[0] == "schema" {
        let mut stmt = conn.prepare("SELECT sql FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
        let mut sqls = Vec::new();
        for r in rows.flatten() {
            sqls.push(r);
        }
        return Ok(sqls.join(";\n\n") + ";");
    }

    let query = args.join(" ");
    let query_trimmed = query.trim();
    let query_upper = query_trimmed.to_uppercase();

    if query_upper.starts_with("SELECT") || query_upper.starts_with("PRAGMA") || query_upper.starts_with("EXPLAIN") {
        let mut stmt = conn.prepare(query_trimmed).map_err(|e| format!("sqlite error: {e}"))?;
        let col_count = stmt.column_count();
        let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();

        let mut rows_data: Vec<Vec<String>> = Vec::new();
        let mut query_rows = stmt.query([]).map_err(|e| format!("sqlite error: {e}"))?;

        while let Some(row) = query_rows.next().map_err(|e| format!("sqlite error: {e}"))? {
            let mut row_vals = Vec::new();
            for i in 0..col_count {
                let val_str = match row.get_ref(i) {
                    Ok(rusqlite::types::ValueRef::Null) => "NULL".to_string(),
                    Ok(rusqlite::types::ValueRef::Integer(v)) => v.to_string(),
                    Ok(rusqlite::types::ValueRef::Real(v)) => format!("{:.2}", v),
                    Ok(rusqlite::types::ValueRef::Text(t)) => String::from_utf8_lossy(t).to_string(),
                    Ok(rusqlite::types::ValueRef::Blob(_)) => "<BLOB>".to_string(),
                    Err(_) => "?".to_string(),
                };
                row_vals.push(val_str);
            }
            rows_data.push(row_vals);
        }

        if rows_data.is_empty() {
            return Ok(format!("(0 rows returned)\nColumns: {}", col_names.join(", ")));
        }

        let mut widths: Vec<usize> = col_names.iter().map(|c| c.chars().count()).collect();
        for row in &rows_data {
            for (i, val) in row.iter().enumerate() {
                widths[i] = widths[i].max(val.chars().count());
            }
        }

        let mut out = String::new();
        out.push('┌');
        for (i, w) in widths.iter().enumerate() {
            out.push_str(&"─".repeat(*w + 2));
            if i + 1 < widths.len() {
                out.push('┬');
            }
        }
        out.push_str("┐\n│");
        for (i, (name, w)) in col_names.iter().zip(&widths).enumerate() {
            out.push_str(&format!(" {:<width$} ", name, width = *w));
            if i + 1 < widths.len() {
                out.push('│');
            }
        }
        out.push_str("│\n├");
        for (i, w) in widths.iter().enumerate() {
            out.push_str(&"─".repeat(*w + 2));
            if i + 1 < widths.len() {
                out.push('┼');
            }
        }
        out.push_str("┤\n");

        for row in &rows_data {
            out.push('│');
            for (i, (val, w)) in row.iter().zip(&widths).enumerate() {
                out.push_str(&format!(" {:<width$} ", val, width = *w));
                if i + 1 < widths.len() {
                    out.push('│');
                }
            }
            out.push_str("│\n");
        }

        out.push('└');
        for (i, w) in widths.iter().enumerate() {
            out.push_str(&"─".repeat(*w + 2));
            if i + 1 < widths.len() {
                out.push('┴');
            }
        }
        out.push_str("┘\n");
        out.push_str(&format!("({} row{} returned)", rows_data.len(), if rows_data.len() == 1 { "" } else { "s" }));

        Ok(out)
    } else {
        let affected = conn.execute(query_trimmed, []).map_err(|e| format!("sqlite error: {e}"))?;
        Ok(format!("Query executed successfully ({} row{} affected).", affected, if affected == 1 { "" } else { "s" }))
    }
}

pub fn run_ping_command(args: &[String]) -> String {
    let mut count = 3;
    let mut host = "localhost";
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-c" && i + 1 < args.len() {
            count = args[i + 1].parse::<usize>().unwrap_or(3).clamp(1, 10);
            i += 2;
        } else if !args[i].starts_with('-') {
            host = &args[i];
            i += 1;
        } else {
            i += 1;
        }
    }

    let ip = match host {
        "localhost" | "127.0.0.1" => "127.0.0.1",
        "gateway" | "router" => "192.168.1.1",
        "internal.api" => "10.0.4.15",
        "dns" | "8.8.8.8" => "8.8.8.8",
        _ => "192.0.2.1",
    };

    let is_unreachable = host.contains("dead") || host.contains("unreachable") || host.contains("down") || host.contains("stale");

    let mut out = String::new();
    out.push_str(&format!("PING {host} ({ip}) 56(84) bytes of data.\n"));

    if is_unreachable {
        for seq in 1..=count {
            out.push_str(&format!("From 10.254.0.1 icmp_seq={seq} Destination Host Unreachable\n"));
        }
        out.push_str(&format!("\n--- {host} ping statistics ---\n"));
        out.push_str(&format!("{count} packets transmitted, 0 received, +{count} errors, 100% packet loss\n"));
    } else {
        let base_times = [0.038, 0.042, 0.035, 0.040, 0.037, 0.044, 0.039, 0.036, 0.041, 0.038];
        for seq in 1..=count {
            let latency = base_times[(seq - 1) % base_times.len()];
            out.push_str(&format!("64 bytes from {ip}: icmp_seq={seq} ttl=64 time={latency:.3} ms\n"));
        }
        out.push_str(&format!("\n--- {host} ping statistics ---\n"));
        out.push_str(&format!("{count} packets transmitted, {count} received, 0% packet loss, time {}ms\n", count * 1000 + 4));
        out.push_str("rtt min/avg/max/mdev = 0.035/0.039/0.044/0.003 ms");
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_lifecycle_start_stop_restart() {
        let path = std::env::temp_dir().join(format!("shellpilot_test_svc1_{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        let sim = ServerSimulator::new();

        assert!(!sim.is_web_running());

        // Start
        let start_res = sim.handle_service_command(&["start".into(), "web".into()], &path);
        assert!(start_res.is_ok());
        assert!(sim.is_web_running());

        // Status
        let status = sim.status_overview(Some("web"));
        assert!(status.contains("active (running)"));

        // Stop
        let stop_res = sim.handle_service_command(&["stop".into(), "web".into()], &path);
        assert!(stop_res.is_ok());
        assert!(!sim.is_web_running());

        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn web_service_crashes_on_corrupted_config() {
        let path = std::env::temp_dir().join(format!("shellpilot_test_svc2_{}", std::process::id()));
        fs::create_dir_all(path.join("config")).unwrap();
        fs::write(path.join("config/server.conf"), "port = NaN_PORT_CRASH\n").unwrap();

        let sim = ServerSimulator::new();
        let res = sim.handle_service_command(&["start".into(), "web".into()], &path);
        assert!(res.is_err());
        assert!(!sim.is_web_running());

        let logs = sim.get_service_logs("web").unwrap();
        assert!(logs.contains("NaN_PORT_CRASH"));

        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn worker_crashes_on_stale_lockfile() {
        let path = std::env::temp_dir().join(format!("shellpilot_test_svc3_{}", std::process::id()));
        fs::create_dir_all(path.join("services")).unwrap();
        fs::write(path.join("services/worker.lock"), "19842\n").unwrap();

        let sim = ServerSimulator::new();
        let res = sim.handle_service_command(&["start".into(), "worker".into()], &path);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("Lockfile exists"));

        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn mock_curl_refused_when_stopped_and_succeeds_when_running() {
        let path = std::env::temp_dir().join(format!("shellpilot_test_svc4_{}", std::process::id()));
        fs::create_dir_all(path.join("data")).unwrap();
        fs::write(
            path.join("data/customers.csv"),
            "id,name,email,plan,status\n1001,Alice Chen,alice@example.com,enterprise,active\n",
        )
        .unwrap();

        let sim = ServerSimulator::new();

        // Stopped -> Connection refused
        let curl_stopped = sim.handle_curl(&["http://localhost:8080/health".into()], &path);
        assert!(curl_stopped.is_err());
        assert!(curl_stopped.unwrap_err().contains("Connection refused"));

        // Start service
        sim.start_service("web", &path).unwrap();

        // Health
        let health = sim.handle_curl(&["http://localhost:8080/health".into()], &path).unwrap();
        assert!(health.contains("\"status\": \"UP\""));

        // Customers
        let cust = sim.handle_curl(&["http://localhost:8080/api/customers".into()], &path).unwrap();
        assert!(cust.contains("Alice Chen"));
        assert!(cust.contains("enterprise"));

        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn db_command_initializes_and_queries_tables() {
        let path = std::env::temp_dir().join(format!("shellpilot_test_db_{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();

        // Overview / Help
        let overview = run_db_command(&path, &[]).unwrap();
        assert!(overview.contains("SHELLPILOT EMBEDDED SQL DATABASE"));
        assert!(overview.contains("customers"));

        // Schema
        let schema = run_db_command(&path, &["schema".into()]).unwrap();
        assert!(schema.contains("CREATE TABLE customers"));

        // Query
        let query_res = run_db_command(&path, &["SELECT name, plan FROM customers WHERE plan = 'enterprise'".into()]).unwrap();
        assert!(query_res.contains("Alice Chen"));
        assert!(query_res.contains("enterprise"));
        assert!(query_res.contains("Dana Scully"));

        // Modifying query
        let insert_res = run_db_command(&path, &["INSERT INTO customers (id, name, email, plan, status) VALUES (9999, 'Test User', 't@t.com', 'pro', 'active')".into()]).unwrap();
        assert!(insert_res.contains("1 row affected"));

        fs::remove_dir_all(&path).ok();
    }

    #[test]
    fn ping_command_simulates_icmp_output() {
        let ping_local = run_ping_command(&["-c".into(), "2".into(), "localhost".into()]);
        assert!(ping_local.contains("PING localhost"));
        assert!(ping_local.contains("64 bytes from 127.0.0.1"));
        assert!(ping_local.contains("2 packets transmitted, 2 received"));

        let ping_down = run_ping_command(&["-c".into(), "2".into(), "dead-host.internal".into()]);
        assert!(ping_down.contains("Destination Host Unreachable"));
        assert!(ping_down.contains("100% packet loss"));
    }
}
