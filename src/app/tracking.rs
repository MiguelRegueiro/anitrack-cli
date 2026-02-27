use std::collections::HashMap;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitStatus, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use super::episode::{
    fetch_episode_labels, parse_title_and_total_eps, previous_seed_episode,
    previous_target_episode, replay_seed_episode, sanitize_title_for_search,
};
use crate::db::{Database, SeenEntry};
use anyhow::{Context, Result, anyhow};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct HistEntry {
    pub(crate) ep: String,
    pub(crate) id: String,
    pub(crate) title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HistFileSig {
    pub(crate) len: u64,
    pub(crate) modified_ns: u128,
}

#[derive(Debug, Clone)]
pub(crate) struct PlaybackOutcome {
    pub(crate) success: bool,
    pub(crate) final_episode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReplayPlan {
    Continue {
        seed_episode: String,
    },
    Episode {
        episode: String,
        select_nth: Option<u32>,
    },
}

#[cfg(unix)]
pub(crate) fn with_sigint_ignored<F, R>(f: F) -> Result<R>
where
    F: FnOnce() -> Result<R>,
{
    unsafe {
        let mut new_action: libc::sigaction = std::mem::zeroed();
        new_action.sa_sigaction = libc::SIG_IGN;
        libc::sigemptyset(&mut new_action.sa_mask);
        new_action.sa_flags = 0;

        let mut old_action: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGINT, &new_action, &mut old_action) != 0 {
            return Err(anyhow!("failed to ignore SIGINT"));
        }

        let result = f();
        let _ = libc::sigaction(libc::SIGINT, &old_action, std::ptr::null_mut());
        result
    }
}

#[cfg(not(unix))]
pub(crate) fn with_sigint_ignored<F, R>(f: F) -> Result<R>
where
    F: FnOnce() -> Result<R>,
{
    f()
}

#[cfg(unix)]
pub(crate) fn run_interactive_cmd(mut cmd: ProcessCommand) -> Result<ExitStatus> {
    let stdin_fd = libc::STDIN_FILENO;
    let parent_pgrp = unsafe { libc::tcgetpgrp(stdin_fd) };
    if parent_pgrp == -1 {
        return Err(anyhow!("failed to read terminal process group"));
    }

    unsafe {
        let _ = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
    }

    unsafe {
        cmd.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGQUIT, libc::SIG_DFL);
            libc::signal(libc::SIGTSTP, libc::SIG_DFL);
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn().context("failed to spawn ani-cli")?;
    let child_pgid = child.id() as libc::pid_t;
    unsafe {
        let _ = libc::tcsetpgrp(stdin_fd, child_pgid);
    }

    let status = child.wait().context("failed waiting on ani-cli")?;

    unsafe {
        let _ = libc::tcsetpgrp(stdin_fd, parent_pgrp);
        let _ = libc::signal(libc::SIGTTOU, libc::SIG_DFL);
    }

    Ok(status)
}

#[cfg(not(unix))]
pub(crate) fn run_interactive_cmd(mut cmd: ProcessCommand) -> Result<ExitStatus> {
    cmd.status().context("failed to launch ani-cli")
}

pub(crate) fn run_ani_cli_search(db: &Database) -> Result<(String, Option<String>)> {
    let histfile = ani_cli_histfile();
    let before_sig = read_histfile_sig(&histfile);
    let before_read = read_hist_map(&histfile);
    let before = before_read.entries;
    let before_ordered = before_read.ordered_entries;
    let mut warnings = before_read.warnings;
    let log_window_start_ns = unix_now_ns();

    let ani_cli_bin = resolve_ani_cli_bin();
    let status = match with_sigint_ignored(|| {
        let mut cmd = ProcessCommand::new(&ani_cli_bin);
        cmd.stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        run_interactive_cmd(cmd)
            .with_context(|| format!("failed to launch {}", ani_cli_bin.display()))
    }) {
        Ok(status) => status,
        Err(err) => {
            let mut message = format!("ani-cli failed to start: {err}. Progress unchanged.");
            append_history_warnings(&mut message, &warnings);
            return Ok((message, None));
        }
    };

    let after_read = read_hist_map(&histfile);
    let after_sig = read_histfile_sig(&histfile);
    let log_window_end_ns = unix_now_ns();
    warnings.extend(after_read.warnings);
    let after_ordered = after_read.ordered_entries;
    let mut changed_id = None;
    let changed =
        detect_latest_watch_event(&before, &before_ordered, &after_ordered).or_else(|| {
            detect_latest_watch_event_from_logs(
                log_window_start_ns,
                log_window_end_ns,
                &after_ordered,
            )
        });
    let mut message = if let Some(changed) = changed {
        db.upsert_seen(&changed.id, &changed.title, &changed.ep)?;
        changed_id = Some(changed.id);
        format!(
            "Recorded last seen: {} | episode {}",
            changed.title, changed.ep
        )
    } else if history_file_touched(before_sig, after_sig) && before_ordered != after_ordered {
        "History changed but no parseable watch entry was detected from this run.".to_string()
    } else {
        "No new history entry detected from this run.".to_string()
    };

    if !status.success() {
        message = format!("{message}\nani-cli exited with status: {status}");
    }

    append_history_warnings(&mut message, &warnings);
    Ok((message, changed_id))
}

pub(crate) fn resolve_ani_cli_bin() -> PathBuf {
    PathBuf::from("ani-cli")
}

pub(crate) fn run_ani_cli_continue(
    item: &SeenEntry,
    stored_episode: &str,
) -> Result<PlaybackOutcome> {
    let temp_hist_dir = make_temp_hist_dir()?;
    let histfile = temp_hist_dir.join("ani-hsts");
    fs::write(
        &histfile,
        format!("{stored_episode}\t{}\t{}\n", item.ani_id, item.title),
    )
    .with_context(|| {
        format!(
            "failed writing temp ani-cli history at {}",
            histfile.display()
        )
    })?;

    let ani_cli_bin = resolve_ani_cli_bin();
    let status = ProcessCommand::new(&ani_cli_bin)
        .arg("-c")
        .env("ANI_CLI_HIST_DIR", &temp_hist_dir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to launch {}", ani_cli_bin.display()))?;
    let final_episode = if status.success() {
        let hist_read = read_hist_map(&histfile);
        for warning in hist_read.warnings {
            eprintln!("Warning: {warning}");
        }
        hist_read
            .entries
            .get(&item.ani_id)
            .map(|entry| entry.ep.clone())
    } else {
        None
    };
    let _ = fs::remove_dir_all(&temp_hist_dir);
    Ok(PlaybackOutcome {
        success: status.success(),
        final_episode,
    })
}

pub(crate) fn run_ani_cli_episode(
    title: &str,
    select_nth: Option<u32>,
    episode: &str,
) -> Result<bool> {
    let ani_cli_bin = resolve_ani_cli_bin();
    let mut cmd = ProcessCommand::new(&ani_cli_bin);
    if let Some(index) = select_nth {
        cmd.arg("-S").arg(index.to_string());
    }
    let status = cmd
        .arg(title)
        .arg("-e")
        .arg(episode)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to launch {}", ani_cli_bin.display()))?;
    Ok(status.success())
}

pub(crate) fn run_ani_cli_title(title: &str, select_nth: Option<u32>) -> Result<bool> {
    let ani_cli_bin = resolve_ani_cli_bin();
    let mut cmd = ProcessCommand::new(&ani_cli_bin);
    if let Some(index) = select_nth {
        cmd.arg("-S").arg(index.to_string());
    }
    let status = cmd
        .arg(title)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to launch {}", ani_cli_bin.display()))?;
    Ok(status.success())
}

pub(crate) fn run_ani_cli_episode_with_global_tracking(
    item: &SeenEntry,
    episode: &str,
    select_nth: Option<u32>,
) -> Result<PlaybackOutcome> {
    let histfile = ani_cli_histfile();
    let before_read = read_hist_map(&histfile);
    for warning in before_read.warnings {
        eprintln!("Warning: {warning}");
    }
    let before = before_read.entries;
    let success =
        run_ani_cli_episode(&sanitize_title_for_search(&item.title), select_nth, episode)?;
    let final_episode = if success {
        let after_read = read_hist_map(&histfile);
        for warning in after_read.warnings {
            eprintln!("Warning: {warning}");
        }
        after_read
            .entries
            .get(&item.ani_id)
            .or_else(|| before.get(&item.ani_id))
            .map(|entry| entry.ep.clone())
    } else {
        None
    };

    Ok(PlaybackOutcome {
        success,
        final_episode,
    })
}

pub(crate) fn run_ani_cli_select(item: &SeenEntry) -> Result<PlaybackOutcome> {
    let histfile = ani_cli_histfile();
    let before_read = read_hist_map(&histfile);
    for warning in before_read.warnings {
        eprintln!("Warning: {warning}");
    }
    let before = before_read.entries;
    let select_nth = resolve_select_nth_for_item(item)
        .ok_or_else(|| anyhow!("failed to resolve current show for episode selection"))?;
    let success = run_ani_cli_title(&sanitize_title_for_search(&item.title), Some(select_nth))?;
    let final_episode = if success {
        let after_read = read_hist_map(&histfile);
        for warning in after_read.warnings {
            eprintln!("Warning: {warning}");
        }
        after_read
            .entries
            .get(&item.ani_id)
            .or_else(|| before.get(&item.ani_id))
            .map(|entry| entry.ep.clone())
    } else {
        None
    };

    Ok(PlaybackOutcome {
        success,
        final_episode,
    })
}

pub(crate) fn resolve_select_nth_for_item(item: &SeenEntry) -> Option<u32> {
    let cleaned_title = sanitize_title_for_search(&item.title);
    let raw_title = item.title.trim().to_string();
    let queries = if cleaned_title == raw_title {
        vec![cleaned_title]
    } else {
        vec![cleaned_title, raw_title]
    };

    let env_mode = env::var("ANI_CLI_MODE").unwrap_or_else(|_| "sub".to_string());
    let mut modes = vec![env_mode, "sub".to_string(), "dub".to_string()];
    modes.dedup();

    for query in queries {
        for mode in &modes {
            let Some(entries) = fetch_search_result_entries(&query, mode) else {
                continue;
            };
            if let Some(index) = find_select_nth_index_by_id(&entries, &item.ani_id) {
                return Some(index);
            }
            if let Some(index) = find_select_nth_index_by_title(&entries, &item.title) {
                return Some(index);
            }
        }
    }
    None
}

pub(crate) fn fetch_search_result_entries(
    query: &str,
    mode: &str,
) -> Option<Vec<SearchResultEntry>> {
    let gql = "query( $search: SearchInput $limit: Int $page: Int $translationType: VaildTranslationTypeEnumType $countryOrigin: VaildCountryOriginEnumType ) { shows( search: $search limit: $limit page: $page translationType: $translationType countryOrigin: $countryOrigin ) { edges { _id name availableEpisodes __typename } }}";
    let escaped_query = json_escape(query);
    let escaped_mode = json_escape(mode);
    let variables = format!(
        "{{\"search\":{{\"allowAdult\":false,\"allowUnknown\":false,\"query\":\"{escaped_query}\"}},\"limit\":40,\"page\":1,\"translationType\":\"{escaped_mode}\",\"countryOrigin\":\"ALL\"}}"
    );
    let output = ProcessCommand::new("curl")
        .arg("-e")
        .arg("https://allmanga.to")
        .arg("-sS")
        .arg("--connect-timeout")
        .arg("3")
        .arg("--max-time")
        .arg("6")
        .arg("-G")
        .arg("https://api.allanime.day/api")
        .arg("--data-urlencode")
        .arg(format!("variables={variables}"))
        .arg("--data-urlencode")
        .arg(format!("query={gql}"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let raw = String::from_utf8(output.stdout).ok()?;
    let entries = parse_search_result_entries(&raw);
    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SearchResultEntry {
    pub(crate) id: String,
    pub(crate) title: String,
}

pub(crate) fn parse_search_result_entries(raw: &str) -> Vec<SearchResultEntry> {
    let mut entries = Vec::new();
    let marker = "\"_id\":\"";
    let mut cursor = 0usize;

    while let Some(rel_start) = raw[cursor..].find(marker) {
        let id_start = cursor + rel_start + marker.len();
        let Some((id, id_end)) = parse_json_string(raw, id_start) else {
            break;
        };
        let Some(rel_name_marker) = raw[id_end..].find("\"name\":\"") else {
            cursor = id_end;
            continue;
        };
        let name_start = id_end + rel_name_marker + "\"name\":\"".len();
        let Some((title, title_end)) = parse_json_string(raw, name_start) else {
            break;
        };
        entries.push(SearchResultEntry { id, title });
        cursor = title_end;
    }

    entries
}

pub(crate) fn parse_json_string(raw: &str, start: usize) -> Option<(String, usize)> {
    let bytes = raw.as_bytes();
    let mut i = start;
    let mut out = String::new();
    let mut escaped = false;
    while i < bytes.len() {
        let b = bytes[i];
        if escaped {
            out.push(match b {
                b'"' => '"',
                b'\\' => '\\',
                b'n' => '\n',
                b'r' => '\r',
                b't' => '\t',
                _ => b as char,
            });
            escaped = false;
            i += 1;
            continue;
        }

        if b == b'\\' {
            escaped = true;
            i += 1;
            continue;
        }
        if b == b'"' {
            return Some((out, i + 1));
        }
        out.push(b as char);
        i += 1;
    }
    None
}

pub(crate) fn find_select_nth_index_by_id(
    entries: &[SearchResultEntry],
    ani_id: &str,
) -> Option<u32> {
    entries
        .iter()
        .position(|entry| entry.id == ani_id)
        .map(|idx| (idx + 1) as u32)
}

pub(crate) fn find_select_nth_index_by_title(
    entries: &[SearchResultEntry],
    title: &str,
) -> Option<u32> {
    let target = normalize_title_for_match(title);
    entries
        .iter()
        .position(|entry| normalize_title_for_match(&entry.title) == target)
        .map(|idx| (idx + 1) as u32)
}

pub(crate) fn normalize_title_for_match(raw: &str) -> String {
    let base = parse_title_and_total_eps(raw).0;
    base.to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch.is_whitespace() {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn json_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let code = c as u32;
                out.push_str(&format!("\\u{code:04x}"));
            }
            c => out.push(c),
        }
    }
    out
}

pub(crate) fn run_ani_cli_replay(
    item: &SeenEntry,
    episode_list: Option<&[String]>,
) -> Result<PlaybackOutcome> {
    let fetched_episodes;
    let resolved_episode_list = if episode_list.is_some() {
        episode_list
    } else {
        let total_hint = parse_title_and_total_eps(&item.title).1;
        fetched_episodes = fetch_episode_labels(&item.ani_id, total_hint);
        fetched_episodes.as_deref()
    };

    let plan = build_replay_plan(item, resolved_episode_list, resolve_select_nth_for_item);
    match plan {
        ReplayPlan::Continue { seed_episode } => run_ani_cli_continue(item, &seed_episode),
        ReplayPlan::Episode {
            episode,
            select_nth,
        } => run_ani_cli_episode_with_global_tracking(item, &episode, select_nth),
    }
}

pub(crate) fn build_replay_plan<F>(
    item: &SeenEntry,
    episode_list: Option<&[String]>,
    resolve_select_nth: F,
) -> ReplayPlan
where
    F: FnOnce(&SeenEntry) -> Option<u32>,
{
    if let Some(seed_episode) = replay_seed_episode(&item.last_episode, episode_list) {
        ReplayPlan::Continue { seed_episode }
    } else {
        // Episode 0 / first-entry replay can otherwise open ambiguous show search in ani-cli.
        ReplayPlan::Episode {
            episode: item.last_episode.clone(),
            select_nth: resolve_select_nth(item),
        }
    }
}

pub(crate) fn run_ani_cli_previous(
    item: &SeenEntry,
    episode_list: Option<&[String]>,
) -> Result<PlaybackOutcome> {
    let fetched_episodes;
    let resolved_episode_list = if episode_list.is_some() {
        episode_list
    } else {
        let total_hint = parse_title_and_total_eps(&item.title).1;
        fetched_episodes = fetch_episode_labels(&item.ani_id, total_hint);
        fetched_episodes.as_deref()
    };

    let target_episode = previous_target_episode(&item.last_episode, resolved_episode_list)
        .ok_or_else(|| anyhow!("no previous episode available"))?;
    if let Some(seed_episode) = previous_seed_episode(&item.last_episode, resolved_episode_list) {
        run_ani_cli_continue(item, &seed_episode)
    } else {
        let select_nth = resolve_select_nth_for_item(item)
            .ok_or_else(|| anyhow!("failed to resolve current show for previous action"))?;
        run_ani_cli_episode_with_global_tracking(item, &target_episode, Some(select_nth))
    }
}

pub(crate) fn make_temp_hist_dir() -> Result<PathBuf> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = env::temp_dir().join(format!("anitrack-hist-{}-{ts}", std::process::id()));
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create temp history dir {}", dir.display()))?;
    Ok(dir)
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

#[derive(Debug, Default)]
struct HistRead {
    entries: HashMap<String, HistEntry>,
    ordered_entries: Vec<HistEntry>,
    warnings: Vec<String>,
}

fn read_hist_map(path: &Path) -> HistRead {
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
