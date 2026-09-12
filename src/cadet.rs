use rusqlite::Connection;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CadetRank {
    Recruit,
    Cadet,
    JuniorPlumber,
    SystemsGuardian,
    DetectiveInspector,
    IncidentFirstResponder,
    MasterFlightCommander,
}

impl CadetRank {
    pub fn title(&self) -> &'static str {
        match self {
            CadetRank::Recruit => "Recruit",
            CadetRank::Cadet => "Flight Cadet",
            CadetRank::JuniorPlumber => "Junior Plumber",
            CadetRank::SystemsGuardian => "Systems Guardian",
            CadetRank::DetectiveInspector => "Detective Inspector",
            CadetRank::IncidentFirstResponder => "Incident First Responder",
            CadetRank::MasterFlightCommander => "Master Flight Commander",
        }
    }

    pub fn min_xp(&self) -> u32 {
        match self {
            CadetRank::Recruit => 0,
            CadetRank::Cadet => 100,
            CadetRank::JuniorPlumber => 250,
            CadetRank::SystemsGuardian => 500,
            CadetRank::DetectiveInspector => 800,
            CadetRank::IncidentFirstResponder => 1200,
            CadetRank::MasterFlightCommander => 1800,
        }
    }

    pub fn next_rank(&self) -> Option<CadetRank> {
        match self {
            CadetRank::Recruit => Some(CadetRank::Cadet),
            CadetRank::Cadet => Some(CadetRank::JuniorPlumber),
            CadetRank::JuniorPlumber => Some(CadetRank::SystemsGuardian),
            CadetRank::SystemsGuardian => Some(CadetRank::DetectiveInspector),
            CadetRank::DetectiveInspector => Some(CadetRank::IncidentFirstResponder),
            CadetRank::IncidentFirstResponder => Some(CadetRank::MasterFlightCommander),
            CadetRank::MasterFlightCommander => None,
        }
    }
}

pub struct Badge {
    pub id: &'static str,
    pub title: &'static str,
    pub icon: &'static str,
    pub description: &'static str,
}

pub const ALL_BADGES: &[Badge] = &[
    Badge {
        id: "cadet_nav",
        title: "Cartographer",
        icon: "🧭",
        description: "Completed all Filesystem & Navigation challenges",
    },
    Badge {
        id: "cadet_pipe",
        title: "Pipeline Alchemist",
        icon: "🌊",
        description: "Mastered pipes, redirections, and stream filters",
    },
    Badge {
        id: "cadet_perm",
        title: "Guardian of Permissions",
        icon: "🛡️",
        description: "Hardened scripts, keys, and confidential files",
    },
    Badge {
        id: "cadet_find",
        title: "Master Detective",
        icon: "🕵️",
        description: "Solved all search, inspection, and grep challenges",
    },
    Badge {
        id: "cadet_inc",
        title: "First Responder",
        icon: "🚒",
        description: "Resolved all emergency server incident challenges",
    },
    Badge {
        id: "time_lord",
        title: "Time Weaver",
        icon: "⏳",
        description: "Utilized both dry-run 'whatif' and time-travel 'undo'",
    },
    Badge {
        id: "drill_hero",
        title: "Drill Specialist",
        icon: "🚨",
        description: "Successfully resolved emergency chaos drills",
    },
    Badge {
        id: "cadet_sre",
        title: "SRE Commander",
        icon: "🛡️",
        description: "Mastered microservices, diagnostics, and recovery",
    },
    Badge {
        id: "cadet_data",
        title: "Data Alchemist",
        icon: "🔮",
        description: "Mastered structured data pipelines and transmutations",
    },
    Badge {
        id: "db_sorcerer",
        title: "Database Sorcerer",
        icon: "🗄️",
        description: "Queried and managed the embedded SQL database",
    },
    Badge {
        id: "netrunner",
        title: "Netrunner",
        icon: "🌐",
        description: "Tested network sockets and latency with ping and netstat",
    },
];

pub struct CadetProfile {
    pub xp: u32,
    pub commands_run: u32,
    pub completed_lessons: HashSet<String>,
    pub completed_drills: HashSet<String>,
    pub undos_used: u32,
    pub whatifs_used: u32,
    pub db_queries_run: u32,
    pub network_probes_run: u32,
}

impl CadetProfile {
    pub fn new(db: Option<&Connection>) -> Self {
        let mut profile = Self {
            xp: 0,
            commands_run: 0,
            completed_lessons: HashSet::new(),
            completed_drills: HashSet::new(),
            undos_used: 0,
            whatifs_used: 0,
            db_queries_run: 0,
            network_probes_run: 0,
        };

        if let Some(conn) = db {
            profile.init_db(conn);
            profile.load_db(conn);
        }

        profile
    }

    fn init_db(&self, conn: &Connection) {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS cadet_stats (
                stat_key TEXT PRIMARY KEY,
                stat_val INTEGER
            );
            CREATE TABLE IF NOT EXISTS cadet_drills (
                drill_id TEXT PRIMARY KEY,
                completed_at INTEGER
            );",
            [],
        )
        .ok();
    }

    fn load_db(&mut self, conn: &Connection) {
        if let Ok(mut stmt) = conn.prepare("SELECT stat_key, stat_val FROM cadet_stats")
            && let Ok(rows) = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))) {
                for r in rows.flatten() {
                    match r.0.as_str() {
                        "xp" => self.xp = r.1 as u32,
                        "commands_run" => self.commands_run = r.1 as u32,
                        "undos_used" => self.undos_used = r.1 as u32,
                        "whatifs_used" => self.whatifs_used = r.1 as u32,
                        "db_queries_run" => self.db_queries_run = r.1 as u32,
                        "network_probes_run" => self.network_probes_run = r.1 as u32,
                        _ => {}
                    }
                }
            }

        if let Ok(mut stmt) = conn.prepare("SELECT drill_id FROM cadet_drills")
            && let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
                for r in rows.flatten() {
                    self.completed_drills.insert(r);
                }
            }

        if let Ok(mut stmt) = conn.prepare("SELECT lesson_id FROM tutor_progress")
            && let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
                for r in rows.flatten() {
                    self.completed_lessons.insert(r);
                }
            }
    }

    pub fn save_stat(&self, key: &str, val: u32, db: Option<&Connection>) {
        if let Some(conn) = db {
            conn.execute(
                "INSERT OR REPLACE INTO cadet_stats (stat_key, stat_val) VALUES (?1, ?2)",
                rusqlite::params![key, val as i64],
            )
            .ok();
        }
    }

    pub fn rank(&self) -> CadetRank {
        if self.xp >= CadetRank::MasterFlightCommander.min_xp() {
            CadetRank::MasterFlightCommander
        } else if self.xp >= CadetRank::IncidentFirstResponder.min_xp() {
            CadetRank::IncidentFirstResponder
        } else if self.xp >= CadetRank::DetectiveInspector.min_xp() {
            CadetRank::DetectiveInspector
        } else if self.xp >= CadetRank::SystemsGuardian.min_xp() {
            CadetRank::SystemsGuardian
        } else if self.xp >= CadetRank::JuniorPlumber.min_xp() {
            CadetRank::JuniorPlumber
        } else if self.xp >= CadetRank::Cadet.min_xp() {
            CadetRank::Cadet
        } else {
            CadetRank::Recruit
        }
    }

    pub fn add_xp(&mut self, amount: u32, db: Option<&Connection>) -> Option<CadetRank> {
        let old_rank = self.rank();
        self.xp += amount;
        self.save_stat("xp", self.xp, db);
        let new_rank = self.rank();
        if new_rank > old_rank {
            Some(new_rank)
        } else {
            None
        }
    }

    pub fn record_command(&mut self, db: Option<&Connection>) {
        self.commands_run += 1;
        self.save_stat("commands_run", self.commands_run, db);
    }

    pub fn record_undo(&mut self, db: Option<&Connection>) {
        self.undos_used += 1;
        self.save_stat("undos_used", self.undos_used, db);
        self.add_xp(15, db);
    }

    pub fn record_whatif(&mut self, db: Option<&Connection>) {
        self.whatifs_used += 1;
        self.save_stat("whatifs_used", self.whatifs_used, db);
        self.add_xp(15, db);
    }

    pub fn record_db_query(&mut self, db: Option<&Connection>) {
        self.db_queries_run += 1;
        self.save_stat("db_queries_run", self.db_queries_run, db);
        self.add_xp(20, db);
    }

    pub fn record_network_probe(&mut self, db: Option<&Connection>) {
        self.network_probes_run += 1;
        self.save_stat("network_probes_run", self.network_probes_run, db);
        self.add_xp(15, db);
    }

    pub fn record_drill_completed(&mut self, drill_id: &str, db: Option<&Connection>) {
        self.completed_drills.insert(drill_id.to_string());
        if let Some(conn) = db {
            conn.execute(
                "INSERT OR REPLACE INTO cadet_drills (drill_id, completed_at) VALUES (?1, ?2)",
                rusqlite::params![drill_id, 1000],
            )
            .ok();
        }
        self.add_xp(100, db);
    }

    pub fn is_badge_unlocked(&self, badge_id: &str) -> bool {
        match badge_id {
            "cadet_nav" => ["nav_01", "nav_02", "nav_03", "nav_04"]
                .iter()
                .all(|id| self.completed_lessons.contains(*id)),
            "cadet_pipe" => ["pipe_01", "pipe_02", "pipe_03", "pipe_04"]
                .iter()
                .all(|id| self.completed_lessons.contains(*id)),
            "cadet_perm" => ["perm_01", "perm_02", "perm_03"]
                .iter()
                .all(|id| self.completed_lessons.contains(*id)),
            "cadet_find" => ["find_01", "find_02", "find_03"]
                .iter()
                .all(|id| self.completed_lessons.contains(*id)),
            "cadet_inc" => ["inc_01", "inc_02", "inc_03", "inc_04"]
                .iter()
                .all(|id| self.completed_lessons.contains(*id)),
            "cadet_sre" => ["sre_01", "sre_02", "sre_03", "sre_04"]
                .iter()
                .all(|id| self.completed_lessons.contains(*id)),
            "cadet_data" => ["data_01", "data_02", "data_03", "data_04"]
                .iter()
                .all(|id| self.completed_lessons.contains(*id)),
            "time_lord" => self.undos_used > 0 && self.whatifs_used > 0,
            "drill_hero" => self.completed_drills.len() >= 2,
            "db_sorcerer" => self.db_queries_run > 0,
            "netrunner" => self.network_probes_run > 0,
            _ => false,
        }
    }

    pub fn render_dossier(&self) -> String {
        let rank = self.rank();
        let mut out = String::new();

        out.push_str("\n\x1b[1;36m╔══════════════════════════════════════════════════════════════════════════╗\x1b[0m\n");
        out.push_str("║                    🎖️  CADET FLIGHT DOSSIER                              ║\n");
        out.push_str("\x1b[1;36m╚══════════════════════════════════════════════════════════════════════════╝\x1b[0m\n\n");

        out.push_str(&format!("👤 \x1b[1mRank        :\x1b[0m \x1b[1;33m{}\x1b[0m\n", rank.title()));
        out.push_str(&format!("⭐ \x1b[1mTotal XP    :\x1b[0m \x1b[1;32m{} XP\x1b[0m\n", self.xp));

        // Progress bar to next rank
        if let Some(next) = rank.next_rank() {
            let current_tier_xp = self.xp.saturating_sub(rank.min_xp());
            let tier_span = next.min_xp() - rank.min_xp();
            let percent = (current_tier_xp * 10).checked_div(tier_span).unwrap_or(10);
            let bar = format!("{}{}", "█".repeat(percent as usize), "░".repeat((10 - percent) as usize));
            out.push_str(&format!(
                "📈 \x1b[1mNext Rank   :\x1b[0m {} [{}] ({} / {} XP)\n",
                next.title(),
                bar,
                self.xp,
                next.min_xp()
            ));
        } else {
            out.push_str("📈 \x1b[1mNext Rank   :\x1b[0m \x1b[1;35mMAX RANK ACHIEVED!\x1b[0m\n");
        }

        out.push('\n');
        out.push_str("\x1b[1;35m📊 Flight Statistics:\x1b[0m\n");
        out.push_str(&format!("   • Commands Executed  : {}\n", self.commands_run));
        out.push_str(&format!("   • Lessons Completed  : {} / 26\n", self.completed_lessons.len()));
        out.push_str(&format!("   • Drills Resolved    : {}\n", self.completed_drills.len()));
        out.push_str(&format!("   • Time-Travel Undos  : {}\n", self.undos_used));
        out.push_str(&format!("   • Dry-Run Previews   : {}\n", self.whatifs_used));
        out.push_str(&format!("   • Database Queries   : {}\n", self.db_queries_run));
        out.push_str(&format!("   • Network Probes     : {}\n", self.network_probes_run));

        out.push('\n');
        out.push_str("\x1b[1;35m🏆 Service Badges:\x1b[0m\n");
        for b in ALL_BADGES {
            let unlocked = self.is_badge_unlocked(b.id);
            let status = if unlocked {
                format!("\x1b[1;32m✓ UNLOCKED\x1b[0m {} {}", b.icon, b.title)
            } else {
                format!("\x1b[0;90m○ LOCKED   {} {}\x1b[0m", b.icon, b.title)
            };
            out.push_str(&format!("   {:<40} - {}\n", status, b.description));
        }

        out.push_str("\n\x1b[0;90m💡 Tip: Advance through lessons ('tutor') and drills ('drill') to earn XP and unlock badges!\x1b[0m\n\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_progresses_with_xp() {
        let mut profile = CadetProfile::new(None);
        assert_eq!(profile.rank(), CadetRank::Recruit);

        let rank_up = profile.add_xp(120, None);
        assert_eq!(rank_up, Some(CadetRank::Cadet));
        assert_eq!(profile.rank(), CadetRank::Cadet);

        profile.add_xp(400, None);
        assert_eq!(profile.rank(), CadetRank::SystemsGuardian);
    }

    #[test]
    fn badges_unlock_when_criteria_met() {
        let mut profile = CadetProfile::new(None);
        assert!(!profile.is_badge_unlocked("cadet_nav"));

        profile.completed_lessons.insert("nav_01".into());
        profile.completed_lessons.insert("nav_02".into());
        profile.completed_lessons.insert("nav_03".into());
        assert!(!profile.is_badge_unlocked("cadet_nav"));

        profile.completed_lessons.insert("nav_04".into());
        assert!(profile.is_badge_unlocked("cadet_nav"));

        // Time lord
        profile.undos_used = 1;
        profile.whatifs_used = 1;
        assert!(profile.is_badge_unlocked("time_lord"));
    }
}

