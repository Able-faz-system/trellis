//! Local agreement-history cache (#484).
//!
//! Every successful `init`, `status` and `milestone-status` call records the
//! agreement ID it touched in a small JSON file, so `trellis history` can
//! list recently used agreements instead of making the user keep track of
//! 64-char hex IDs themselves. This mirrors the frontend's client-side
//! history feature.
//!
//! The file lives at `~/.trellis/history.json` (override with
//! `TRELLIS_HISTORY_FILE`, disable with `TRELLIS_NO_HISTORY=1`). It holds
//! only public, on-chain identifiers (agreement and contract IDs), never key
//! material. Recording is best-effort: a cache failure prints a warning and
//! never fails the command that triggered it.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Oldest entries beyond this count are dropped so the file stays small.
const MAX_ENTRIES: usize = 500;

/// Bumped if the on-disk layout ever changes incompatibly.
const FORMAT_VERSION: u64 = 1;

/// One cached agreement, keyed by `(contract_id, agreement_id)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Lowercase hex agreement ID.
    pub agreement_id: String,
    /// Contract the agreement lives in. The same ID can exist in several
    /// deployments (e.g. testnet and mainnet), so it is part of the key.
    pub contract_id: String,
    /// CLI subcommand of the most recent successful call (`init`, `status`, …).
    pub last_command: String,
    /// Unix seconds of the most recent successful call.
    pub last_used: u64,
}

impl Entry {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "agreement_id": self.agreement_id,
            "contract_id": self.contract_id,
            "last_command": self.last_command,
            "last_used": self.last_used,
            "last_used_utc": format_utc(self.last_used),
        })
    }

    fn from_json(v: &serde_json::Value) -> Option<Self> {
        Some(Entry {
            agreement_id: v["agreement_id"].as_str()?.to_string(),
            contract_id: v["contract_id"].as_str()?.to_string(),
            last_command: v["last_command"].as_str().unwrap_or_default().to_string(),
            last_used: v["last_used"].as_u64()?,
        })
    }
}

/// Default cache location, or `None` when history is disabled or no home
/// directory can be determined.
pub fn default_path() -> Option<PathBuf> {
    if std::env::var("TRELLIS_NO_HISTORY").is_ok_and(|v| !v.is_empty()) {
        return None;
    }
    if let Some(p) = std::env::var_os("TRELLIS_HISTORY_FILE").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".trellis").join("history.json"))
}

/// Read all entries from `path`, most recent first. A missing file is an
/// empty history; a file that can't be parsed is an error, so it is never
/// silently overwritten.
pub fn load(path: &Path) -> Result<Vec<Entry>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read history file {}: {e}", path.display())),
    };
    let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        format!(
            "history file {} is corrupt ({e}); remove it or run `trellis history --clear`",
            path.display()
        )
    })?;
    let mut entries: Vec<Entry> = parsed["entries"]
        .as_array()
        .map(|a| a.iter().filter_map(Entry::from_json).collect())
        .unwrap_or_default();
    entries.sort_by(|a, b| b.last_used.cmp(&a.last_used));
    Ok(entries)
}

/// Write `entries` to `path`, creating the parent directory if needed. The
/// file is written to a sibling temp file and renamed into place, so an
/// interrupted write cannot leave a truncated history behind.
fn save(path: &Path, entries: &[Entry]) -> Result<(), String> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let doc = serde_json::json!({
        "version": FORMAT_VERSION,
        "entries": entries.iter().map(Entry::to_json).collect::<Vec<_>>(),
    });
    let body = serde_json::to_string_pretty(&doc).unwrap_or_default();
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Insert or refresh the entry for `(contract_id, agreement_id)` in the file
/// at `path`, stamping it with `now`.
pub fn record_at(
    path: &Path,
    contract_id: &str,
    agreement_id: &str,
    command: &str,
    now: u64,
) -> Result<(), String> {
    let agreement_id = agreement_id.to_ascii_lowercase();
    let mut entries = load(path)?;
    entries.retain(|e| !(e.agreement_id == agreement_id && e.contract_id == contract_id));
    entries.insert(
        0,
        Entry {
            agreement_id,
            contract_id: contract_id.to_string(),
            last_command: command.to_string(),
            last_used: now,
        },
    );
    entries.truncate(MAX_ENTRIES);
    save(path, &entries)
}

/// Best-effort [`record_at`] against the default path. Failures are printed
/// as a warning (unless `quiet`) and otherwise ignored.
pub fn record(contract_id: &str, agreement_id: &str, command: &str, quiet: bool) {
    let Some(path) = default_path() else {
        return;
    };
    if let Err(e) = record_at(&path, contract_id, agreement_id, command, unix_now()) {
        if !quiet {
            eprintln!("Warning: could not update agreement history: {e}");
        }
    }
}

/// Delete the history file at `path`. Succeeds if it doesn't exist.
pub fn clear(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove {}: {e}", path.display())),
    }
}

/// JSON array form of `entries`, used for the `--json` result.
pub fn entries_json(entries: &[Entry]) -> serde_json::Value {
    serde_json::Value::Array(entries.iter().map(Entry::to_json).collect())
}

/// Plain-text table of `entries`, one per line, most recent first.
pub fn format_table(entries: &[Entry]) -> String {
    if entries.is_empty() {
        return "No agreements in history yet. Successful `init`, `status` and \
                `milestone-status` calls are recorded automatically.\n"
            .to_string();
    }
    let mut out = format!(
        "{:<20}  {:<16}  {:<64}  {}\n",
        "LAST USED (UTC)", "COMMAND", "AGREEMENT ID", "CONTRACT"
    );
    for e in entries {
        out.push_str(&format!(
            "{:<20}  {:<16}  {:<64}  {}\n",
            format_utc(e.last_used),
            e.last_command,
            e.agreement_id,
            e.contract_id
        ));
    }
    out
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Format unix seconds as `YYYY-MM-DD HH:MM:SS` (UTC) without pulling in a
/// date-time crate. Uses Howard Hinnant's `civil_from_days` algorithm.
fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);

    format!("{year:04}-{month:02}-{day:02} {h:02}:{m:02}:{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, not-yet-existing history path unique to this test.
    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "trellis-history-{}-{name}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        dir.join("history.json")
    }

    fn id(c: char) -> String {
        c.to_string().repeat(64)
    }

    const CONTRACT: &str = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    #[test]
    fn missing_file_is_empty_history() {
        assert_eq!(load(&temp_path("missing")).unwrap(), Vec::new());
    }

    #[test]
    fn record_creates_file_and_parent_dir() {
        let path = temp_path("create");
        record_at(&path, CONTRACT, &id('a'), "init", 100).unwrap();
        let entries = load(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].agreement_id, id('a'));
        assert_eq!(entries[0].last_command, "init");
        assert_eq!(entries[0].last_used, 100);
    }

    #[test]
    fn entries_listed_most_recent_first() {
        let path = temp_path("order");
        record_at(&path, CONTRACT, &id('a'), "init", 100).unwrap();
        record_at(&path, CONTRACT, &id('b'), "status", 300).unwrap();
        record_at(&path, CONTRACT, &id('c'), "status", 200).unwrap();
        let ids: Vec<_> = load(&path).unwrap().into_iter().map(|e| e.agreement_id).collect();
        assert_eq!(ids, vec![id('b'), id('c'), id('a')]);
    }

    #[test]
    fn re_recording_refreshes_instead_of_duplicating() {
        let path = temp_path("dedupe");
        record_at(&path, CONTRACT, &id('a'), "init", 100).unwrap();
        record_at(&path, CONTRACT, &id('b'), "status", 200).unwrap();
        // Uppercase input must match the lowercase entry already stored.
        record_at(&path, CONTRACT, &id('A'), "milestone-status", 300).unwrap();
        let entries = load(&path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].agreement_id, id('a'));
        assert_eq!(entries[0].last_command, "milestone-status");
        assert_eq!(entries[0].last_used, 300);
    }

    #[test]
    fn same_agreement_id_on_different_contracts_kept_separately() {
        let path = temp_path("contracts");
        let other = CONTRACT.replace('A', "B");
        record_at(&path, CONTRACT, &id('a'), "init", 100).unwrap();
        record_at(&path, &other, &id('a'), "init", 200).unwrap();
        assert_eq!(load(&path).unwrap().len(), 2);
    }

    #[test]
    fn history_is_capped() {
        let path = temp_path("cap");
        let entries: Vec<Entry> = (0..MAX_ENTRIES as u64)
            .map(|i| Entry {
                agreement_id: format!("{i:064x}"),
                contract_id: CONTRACT.to_string(),
                last_command: "status".to_string(),
                last_used: i,
            })
            .collect();
        save(&path, &entries).unwrap();
        record_at(&path, CONTRACT, &id('f'), "init", 10_000).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), MAX_ENTRIES);
        assert_eq!(loaded[0].agreement_id, id('f'));
        // The oldest entry (last_used = 0) was dropped.
        assert!(loaded.iter().all(|e| e.last_used != 0));
    }

    #[test]
    fn corrupt_file_is_error_and_not_overwritten() {
        let path = temp_path("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load(&path).unwrap_err().contains("corrupt"));
        assert!(record_at(&path, CONTRACT, &id('a'), "init", 1).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    }

    #[test]
    fn clear_removes_file_and_is_idempotent() {
        let path = temp_path("clear");
        record_at(&path, CONTRACT, &id('a'), "init", 1).unwrap();
        clear(&path).unwrap();
        assert!(!path.exists());
        clear(&path).unwrap();
    }

    #[test]
    fn json_output_has_one_object_per_entry() {
        let path = temp_path("json");
        record_at(&path, CONTRACT, &id('a'), "init", 0).unwrap();
        let json = entries_json(&load(&path).unwrap());
        assert_eq!(json[0]["agreement_id"], id('a'));
        assert_eq!(json[0]["contract_id"], CONTRACT);
        assert_eq!(json[0]["last_used"], 0);
        assert_eq!(json[0]["last_used_utc"], "1970-01-01 00:00:00");
    }

    #[test]
    fn table_output_lists_entries_and_handles_empty() {
        assert!(format_table(&[]).contains("No agreements in history yet"));
        let entry = Entry {
            agreement_id: id('a'),
            contract_id: CONTRACT.to_string(),
            last_command: "status".to_string(),
            last_used: 1_700_000_000,
        };
        let table = format_table(&[entry]);
        assert!(table.starts_with("LAST USED (UTC)"));
        assert!(table.contains("2023-11-14 22:13:20"));
        assert!(table.contains(&id('a')));
    }

    #[test]
    fn format_utc_handles_leap_day() {
        // 2024-02-29 12:00:00 UTC
        assert_eq!(format_utc(1_709_208_000), "2024-02-29 12:00:00");
    }
}
