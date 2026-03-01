use std::collections::HashMap;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(target_os = "linux")]
use std::process::Command as ProcessCommand;

use super::{HistEntry, HistFileSig};

#[derive(Default)]
pub(super) struct HistRead {
    pub(super) entries: HashMap<String, HistEntry>,
    pub(super) ordered_entries: Vec<HistEntry>,
    pub(super) warnings: Vec<String>,
}

pub(super) fn read_hist_map(path: &Path) -> HistRead {
    if !path.exists() {
        return HistRead::default();
    }

    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) => {
            return HistRead {
                entries: HashMap::new(),
                ordered_entries: Vec::new(),
                warnings: vec![format!(
                    "failed to read ani-cli history at {}: {}",
                    path.display(),
                    err
                )],
            };
        }
    };

    let (entries, ordered_entries, skipped_lines) = parse_hist_map(&raw);
    let mut warnings = Vec::new();
    if skipped_lines > 0 {
        warnings.push(format!(
            "ignored {skipped_lines} malformed line(s) in {}",
            path.display()
        ));
    }

    HistRead {
        entries,
        ordered_entries,
        warnings,
    }
}

pub(crate) fn ani_cli_histfile() -> PathBuf {
    if let Ok(custom) = env::var("ANI_CLI_HIST_DIR") {
        return PathBuf::from(custom).join("ani-hsts");
    }

    let state_home = env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".local/state")
        });

    state_home.join("ani-cli").join("ani-hsts")
}

pub(crate) fn parse_hist_map(raw: &str) -> (HashMap<String, HistEntry>, Vec<HistEntry>, usize) {
    let mut map = HashMap::new();
    let mut ordered_entries = Vec::new();
    let mut skipped_lines = 0;
    for line in raw.lines() {
        match parse_hist_line(line) {
            Some(entry) => {
                ordered_entries.push(entry.clone());
                map.insert(entry.id.clone(), entry);
            }
            None if !line.trim().is_empty() => skipped_lines += 1,
            None => {}
        }
    }
    (map, ordered_entries, skipped_lines)
}

pub(crate) fn parse_hist_line(line: &str) -> Option<HistEntry> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.contains('\t') {
        let mut parts = trimmed.splitn(3, '\t');
        let ep = parts.next()?.trim();
        let id = parts.next()?.trim();
        let title = parts.next()?.trim();
        if ep.is_empty() || id.is_empty() || title.is_empty() {
            return None;
        }
        return Some(HistEntry {
            ep: ep.to_string(),
            id: id.to_string(),
            title: title.to_string(),
        });
    }

    // Fallback for environments where ani-cli history lines are space-separated.
    let mut parts = trimmed.split_whitespace();
    let ep = parts.next()?.trim();
    let id = parts.next()?.trim();
    let title = parts.collect::<Vec<_>>().join(" ");
    if ep.is_empty() || id.is_empty() || title.trim().is_empty() {
        return None;
    }
    Some(HistEntry {
        ep: ep.to_string(),
        id: id.to_string(),
        title: title.trim().to_string(),
    })
}

pub(crate) fn append_history_warnings(message: &mut String, warnings: &[String]) {
    for warning in warnings {
        message.push_str("\nWarning: ");
        message.push_str(warning);
    }
}

pub(crate) fn detect_changed_latest(
    before: &HashMap<String, HistEntry>,
    after_ordered: &[HistEntry],
) -> Option<HistEntry> {
    // Walk from the most recent history lines to pick the last meaningful change deterministically.
    let mut seen_ids = HashSet::new();
    for current in after_ordered.iter().rev() {
        if !seen_ids.insert(current.id.as_str()) {
            continue;
        }
        match before.get(&current.id) {
            None => return Some(current.clone()),
            Some(prev) if prev.ep != current.ep || prev.title != current.title => {
                return Some(current.clone());
            }
            _ => {}
        }
    }
    None
}

pub(crate) fn added_entries(
    before_ordered: &[HistEntry],
    after_ordered: &[HistEntry],
) -> Vec<HistEntry> {
    let mut before_counts: HashMap<HistEntry, usize> = HashMap::new();
    for entry in before_ordered {
        *before_counts.entry(entry.clone()).or_insert(0) += 1;
    }

    let mut added = Vec::new();
    for entry in after_ordered {
        match before_counts.get_mut(entry) {
            Some(count) if *count > 0 => *count -= 1,
            _ => added.push(entry.clone()),
        }
    }
    added
}

pub(crate) fn detect_latest_added_entry(
    before: &HashMap<String, HistEntry>,
    before_ordered: &[HistEntry],
    after_ordered: &[HistEntry],
) -> Option<HistEntry> {
    let added = added_entries(before_ordered, after_ordered);
    if added.is_empty() {
        return None;
    }

    // Prefer the newest meaningful added line. If added lines are all duplicates,
    // use the newest duplicate so same-episode replays still register.
    for current in added.iter().rev() {
        match before.get(&current.id) {
            None => return Some(current.clone()),
            Some(prev) if prev.ep != current.ep || prev.title != current.title => {
                return Some(current.clone());
            }
            _ => {}
        };
    }
    added.last().cloned()
}

pub(crate) fn detect_latest_watch_event(
    before: &HashMap<String, HistEntry>,
    before_ordered: &[HistEntry],
    after_ordered: &[HistEntry],
) -> Option<HistEntry> {
    detect_latest_added_entry(before, before_ordered, after_ordered)
        .or_else(|| detect_changed_latest(before, after_ordered))
}

pub(crate) fn read_histfile_sig(path: &Path) -> Option<HistFileSig> {
    let meta = fs::metadata(path).ok()?;
    let len = meta.len();
    let modified_ns = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(HistFileSig { len, modified_ns })
}

pub(crate) fn history_file_touched(
    before: Option<HistFileSig>,
    after: Option<HistFileSig>,
) -> bool {
    before != after
}

pub(crate) fn unix_now_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(any(test, target_os = "linux"))]
pub(crate) fn parse_short_unix_ts_ns(raw: &str) -> Option<u128> {
    let (secs_raw, frac_raw) = raw.split_once('.').unwrap_or((raw, ""));
    let secs = secs_raw.parse::<u128>().ok()?;
    let mut frac_digits = frac_raw
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>();
    if frac_digits.len() > 9 {
        frac_digits.truncate(9);
    }
    while frac_digits.len() < 9 {
        frac_digits.push('0');
    }
    let frac_ns = if frac_digits.is_empty() {
        0
    } else {
        frac_digits.parse::<u128>().ok()?
    };
    Some(secs.saturating_mul(1_000_000_000).saturating_add(frac_ns))
}

#[cfg(any(test, target_os = "linux"))]
pub(crate) fn parse_journal_ani_cli_line(line: &str) -> Option<(u128, String)> {
    let (ts_raw, rest) = line.split_once(' ')?;
    let ts_ns = parse_short_unix_ts_ns(ts_raw)?;
    let (_, msg) = rest.split_once(": ")?;
    Some((ts_ns, msg.trim().to_string()))
}

#[cfg(any(test, target_os = "linux"))]
pub(crate) fn ani_cli_log_key(title: &str, episode: &str) -> String {
    let title_prefix = title.split('(').next().unwrap_or(title);
    let mut key_raw = String::new();
    key_raw.push_str(title_prefix);
    key_raw.push(' ');
    key_raw.push_str(episode.trim());
    normalize_log_key(&key_raw)
}

#[cfg(any(test, target_os = "linux"))]
pub(crate) fn normalize_log_key(raw: &str) -> String {
    raw.chars()
        .filter(|ch| !ch.is_ascii_punctuation())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(any(test, target_os = "linux"))]
pub(crate) fn detect_log_matched_entry(
    message: &str,
    after_ordered: &[HistEntry],
) -> Option<HistEntry> {
    let target = normalize_log_key(message);
    for entry in after_ordered.iter().rev() {
        if ani_cli_log_key(&entry.title, &entry.ep) == target {
            return Some(entry.clone());
        }
    }
    None
}

#[cfg(target_os = "linux")]
pub(crate) fn detect_latest_watch_event_from_logs(
    start_ns: u128,
    end_ns: u128,
    after_ordered: &[HistEntry],
) -> Option<HistEntry> {
    if after_ordered.is_empty() {
        return None;
    }

    let since_secs = start_ns / 1_000_000_000;
    let until_secs = (end_ns / 1_000_000_000).saturating_add(5);
    let output = ProcessCommand::new("journalctl")
        .arg("-t")
        .arg("ani-cli")
        .arg("--since")
        .arg(format!("@{since_secs}"))
        .arg("--until")
        .arg(format!("@{until_secs}"))
        .arg("--output=short-unix")
        .arg("--no-pager")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let upper_bound_ns = end_ns.saturating_add(5_000_000_000);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut logs = Vec::new();
    for line in stdout.lines() {
        if let Some((ts_ns, msg)) = parse_journal_ani_cli_line(line)
            && ts_ns >= start_ns
            && ts_ns <= upper_bound_ns
        {
            logs.push((ts_ns, msg));
        }
    }

    for (_, message) in logs.iter().rev() {
        if let Some(entry) = detect_log_matched_entry(message, after_ordered) {
            return Some(entry);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn detect_latest_watch_event_from_logs(
    start_ns: u128,
    end_ns: u128,
    after_ordered: &[HistEntry],
) -> Option<HistEntry> {
    let _ = (start_ns, end_ns, after_ordered);
    None
}
