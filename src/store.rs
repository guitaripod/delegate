use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::events::{Envelope, RunEvent};
use crate::packet::Packet;

pub struct Store {
    conn: Connection,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RunRow {
    pub id: String,
    pub packet_id: String,
    pub class: String,
    pub repo: String,
    pub host: String,
    pub mode: String,
    pub start_tier: String,
    pub ceiling: String,
    pub status: String,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub passed_tier: Option<String>,
    pub escalations: u32,
    pub summary: String,
    pub packet: Packet,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AttemptRow {
    pub run_id: String,
    pub tier: String,
    pub chain_index: usize,
    pub runner: String,
    pub model: String,
    pub attempt: u32,
    pub status: String,
    pub verify_exit: Option<i32>,
    pub duration_ms: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub changed_files: Vec<String>,
    pub scope_violations: Vec<String>,
    pub verify_tail: String,
    pub worker_summary: String,
    pub started_at: String,
    pub finished_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StatRow {
    pub class: String,
    pub tier: String,
    pub attempts: u64,
    pub passes: u64,
    pub pass_rate: f64,
    pub avg_ms: f64,
    pub tokens_in: u64,
    pub tokens_out: u64,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS runs (
  id TEXT PRIMARY KEY,
  packet_id TEXT NOT NULL,
  class TEXT NOT NULL,
  repo TEXT NOT NULL,
  host TEXT NOT NULL,
  mode TEXT NOT NULL,
  start_tier TEXT NOT NULL,
  ceiling TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  finished_at TEXT,
  passed_tier TEXT,
  escalations INTEGER NOT NULL DEFAULT 0,
  summary TEXT NOT NULL DEFAULT '',
  packet_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS attempts (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id TEXT NOT NULL REFERENCES runs(id),
  tier TEXT NOT NULL,
  chain_index INTEGER NOT NULL,
  runner TEXT NOT NULL,
  model TEXT NOT NULL,
  attempt INTEGER NOT NULL,
  status TEXT NOT NULL,
  verify_exit INTEGER,
  duration_ms INTEGER NOT NULL,
  tokens_in INTEGER NOT NULL DEFAULT 0,
  tokens_out INTEGER NOT NULL DEFAULT 0,
  changed_files TEXT NOT NULL DEFAULT '[]',
  scope_violations TEXT NOT NULL DEFAULT '[]',
  verify_tail TEXT NOT NULL DEFAULT '',
  worker_summary TEXT NOT NULL DEFAULT '',
  patch BLOB,
  started_at TEXT NOT NULL,
  finished_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS attempts_run ON attempts(run_id);
CREATE TABLE IF NOT EXISTS events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id TEXT NOT NULL,
  seq INTEGER NOT NULL,
  ts TEXT NOT NULL,
  json TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS events_run_seq ON events(run_id, seq);
";

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;",
        )?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn })
    }

    pub fn insert_run(&self, row: &RunRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO runs (id, packet_id, class, repo, host, mode, start_tier, ceiling, status, created_at, escalations, summary, packet_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, '', ?11)",
            params![
                row.id,
                row.packet_id,
                row.class,
                row.repo,
                row.host,
                row.mode,
                row.start_tier,
                row.ceiling,
                row.status,
                row.created_at,
                serde_json::to_string(&row.packet)?,
            ],
        )?;
        Ok(())
    }

    pub fn finish_run(
        &self,
        id: &str,
        status: &str,
        passed_tier: Option<&str>,
        escalations: u32,
        summary: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET status = ?2, finished_at = ?3, passed_tier = ?4, escalations = ?5, summary = ?6 WHERE id = ?1",
            params![id, status, Utc::now().to_rfc3339(), passed_tier, escalations, summary],
        )?;
        Ok(())
    }

    pub fn insert_attempt(&self, row: &AttemptRow, patch: Option<&[u8]>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO attempts (run_id, tier, chain_index, runner, model, attempt, status, verify_exit, duration_ms, tokens_in, tokens_out, changed_files, scope_violations, verify_tail, worker_summary, patch, started_at, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                row.run_id,
                row.tier,
                row.chain_index as i64,
                row.runner,
                row.model,
                row.attempt,
                row.status,
                row.verify_exit,
                row.duration_ms as i64,
                row.tokens_in as i64,
                row.tokens_out as i64,
                serde_json::to_string(&row.changed_files)?,
                serde_json::to_string(&row.scope_violations)?,
                row.verify_tail,
                row.worker_summary,
                patch,
                row.started_at,
                row.finished_at,
            ],
        )?;
        Ok(())
    }

    pub fn append_event(&self, env: &Envelope) -> Result<()> {
        self.conn.execute(
            "INSERT INTO events (run_id, seq, ts, json) VALUES (?1, ?2, ?3, ?4)",
            params![
                env.run_id,
                env.seq as i64,
                env.ts.to_rfc3339(),
                serde_json::to_string(&env.event)?
            ],
        )?;
        Ok(())
    }

    pub fn events(&self, run_id: &str, after_seq: u64) -> Result<Vec<Envelope>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, ts, json FROM events WHERE run_id = ?1 AND seq > ?2 ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![run_id, after_seq as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, ts, json) = row?;
            let event: RunEvent = serde_json::from_str(&json)?;
            let ts: DateTime<Utc> = ts.parse().unwrap_or_else(|_| Utc::now());
            out.push(Envelope {
                run_id: run_id.to_string(),
                seq: seq as u64,
                ts,
                event,
            });
        }
        Ok(out)
    }

    fn run_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunRow> {
        let packet_json: String = r.get(14)?;
        let packet: Packet = serde_json::from_str(&packet_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(14, rusqlite::types::Type::Text, Box::new(e))
        })?;
        Ok(RunRow {
            id: r.get(0)?,
            packet_id: r.get(1)?,
            class: r.get(2)?,
            repo: r.get(3)?,
            host: r.get(4)?,
            mode: r.get(5)?,
            start_tier: r.get(6)?,
            ceiling: r.get(7)?,
            status: r.get(8)?,
            created_at: r.get(9)?,
            finished_at: r.get(10)?,
            passed_tier: r.get(11)?,
            escalations: r.get::<_, i64>(12)? as u32,
            summary: r.get(13)?,
            packet,
        })
    }

    const RUN_COLUMNS: &'static str = "id, packet_id, class, repo, host, mode, start_tier, ceiling, status, created_at, finished_at, passed_tier, escalations, summary, packet_json";

    pub fn list_runs(&self, limit: usize) -> Result<Vec<RunRow>> {
        let sql = format!(
            "SELECT {} FROM runs ORDER BY created_at DESC LIMIT ?1",
            Self::RUN_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![limit as i64], Self::run_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Finds a run by full id or unique prefix.
    pub fn get_run(&self, id_or_prefix: &str) -> Result<RunRow> {
        let sql = format!(
            "SELECT {} FROM runs WHERE id = ?1 OR id LIKE ?2 ORDER BY created_at DESC LIMIT 2",
            Self::RUN_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let pattern = format!("{}%", id_or_prefix.to_uppercase());
        let rows = stmt.query_map(params![id_or_prefix, pattern], Self::run_from_row)?;
        let rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        match rows.len() {
            0 => bail!("no run matches '{id_or_prefix}'"),
            1 => Ok(rows.into_iter().next().unwrap()),
            _ => {
                if rows[0].id.eq_ignore_ascii_case(id_or_prefix) {
                    Ok(rows.into_iter().next().unwrap())
                } else {
                    bail!("'{id_or_prefix}' matches more than one run; use a longer prefix")
                }
            }
        }
    }

    pub fn attempts(&self, run_id: &str) -> Result<Vec<AttemptRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, tier, chain_index, runner, model, attempt, status, verify_exit, duration_ms, tokens_in, tokens_out, changed_files, scope_violations, verify_tail, worker_summary, started_at, finished_at
             FROM attempts WHERE run_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![run_id], |r| {
            let changed: String = r.get(11)?;
            let violations: String = r.get(12)?;
            Ok(AttemptRow {
                run_id: r.get(0)?,
                tier: r.get(1)?,
                chain_index: r.get::<_, i64>(2)? as usize,
                runner: r.get(3)?,
                model: r.get(4)?,
                attempt: r.get::<_, i64>(5)? as u32,
                status: r.get(6)?,
                verify_exit: r.get(7)?,
                duration_ms: r.get::<_, i64>(8)? as u64,
                tokens_in: r.get::<_, i64>(9)? as u64,
                tokens_out: r.get::<_, i64>(10)? as u64,
                changed_files: serde_json::from_str(&changed).unwrap_or_default(),
                scope_violations: serde_json::from_str(&violations).unwrap_or_default(),
                verify_tail: r.get(13)?,
                worker_summary: r.get(14)?,
                started_at: r.get(15)?,
                finished_at: r.get(16)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn stats(&self, class: Option<&str>) -> Result<Vec<StatRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.class, a.tier, COUNT(*), SUM(CASE WHEN a.status = 'pass' THEN 1 ELSE 0 END), AVG(a.duration_ms), SUM(a.tokens_in), SUM(a.tokens_out)
             FROM attempts a JOIN runs r ON r.id = a.run_id
             WHERE (?1 IS NULL OR r.class = ?1) AND a.status != 'error'
             GROUP BY r.class, a.tier ORDER BY r.class, a.tier",
        )?;
        let rows = stmt.query_map(params![class], |r| {
            let attempts: i64 = r.get(2)?;
            let passes: i64 = r.get(3)?;
            Ok(StatRow {
                class: r.get(0)?,
                tier: r.get(1)?,
                attempts: attempts as u64,
                passes: passes as u64,
                pass_rate: if attempts > 0 {
                    passes as f64 / attempts as f64
                } else {
                    0.0
                },
                avg_ms: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                tokens_in: r.get::<_, Option<i64>>(5)?.unwrap_or(0) as u64,
                tokens_out: r.get::<_, Option<i64>>(6)?.unwrap_or(0) as u64,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
