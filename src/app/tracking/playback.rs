use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};

use super::super::episode::{
    fetch_episode_labels_with_diagnostics, parse_title_and_total_eps, previous_seed_episode,
    previous_target_episode, replay_seed_episode, sanitize_title_for_search,
};
use super::api::resolve_select_nth_for_item_with_diagnostics;
use super::history::{
    ani_cli_histfile, append_history_warnings, detect_latest_watch_event,
    detect_latest_watch_event_from_logs_with_diagnostics, history_file_touched, read_hist_map,
    read_histfile_sig, unix_now_ns,
};
use super::process::{run_interactive_cmd, with_sigint_ignored};
use super::{PlaybackOutcome, ReplayPlan};
use crate::db::{Database, SeenEntry};

fn emit_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("Warning: {warning}");
    }
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
            let (entry, log_warning) = detect_latest_watch_event_from_logs_with_diagnostics(
                log_window_start_ns,
                log_window_end_ns,
                &after_ordered,
            );
            if let Some(log_warning) = log_warning {
                warnings.push(log_warning);
            }
            entry
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
    resolve_ani_cli_bin_from_env(env::var_os("ANI_TRACK_ANI_CLI_BIN"))
}

pub(crate) fn resolve_ani_cli_bin_from_env(env_value: Option<OsString>) -> PathBuf {
    match env_value {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => PathBuf::from("ani-cli"),
    }
}

pub(crate) fn run_ani_cli_continue(
    item: &SeenEntry,
    stored_episode: &str,
) -> Result<PlaybackOutcome> {
    let temp_hist_dir = TempHistDir::new()?;
    let histfile = temp_hist_dir.histfile_path();
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
        .env("ANI_CLI_HIST_DIR", temp_hist_dir.path())
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
    let resolution = resolve_select_nth_for_item_with_diagnostics(item);
    emit_warnings(&resolution.warnings);
    let select_nth = resolution.index.ok_or_else(|| {
        let mut message = "failed to resolve current show for episode selection".to_string();
        for warning in resolution.warnings {
            message.push_str("\nWarning: ");
            message.push_str(&warning);
        }
        anyhow!(message)
    })?;
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

pub(crate) fn run_ani_cli_replay(
    item: &SeenEntry,
    episode_list: Option<&[String]>,
) -> Result<PlaybackOutcome> {
    // Avoid external metadata fetches when numeric fallback already determines replay plan.
    let should_fetch_episodes =
        episode_list.is_none() && replay_seed_episode(&item.last_episode, None).is_none();
    let fetched_episodes = if should_fetch_episodes {
        let total_hint = parse_title_and_total_eps(&item.title).1;
        let outcome = fetch_episode_labels_with_diagnostics(&item.ani_id, total_hint);
        emit_warnings(&outcome.warnings);
        outcome.episode_list
    } else {
        None
    };
    let resolved_episode_list = episode_list.or(fetched_episodes.as_deref());

    let mut select_warnings = Vec::new();
    let plan = build_replay_plan(item, resolved_episode_list, |current_item| {
        let resolution = resolve_select_nth_for_item_with_diagnostics(current_item);
        select_warnings = resolution.warnings;
        resolution.index
    });
    emit_warnings(&select_warnings);
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
        let outcome = fetch_episode_labels_with_diagnostics(&item.ani_id, total_hint);
        emit_warnings(&outcome.warnings);
        fetched_episodes = outcome.episode_list;
        fetched_episodes.as_deref()
    };

    let target_episode = previous_target_episode(&item.last_episode, resolved_episode_list)
        .ok_or_else(|| anyhow!("no previous episode available"))?;
    if let Some(seed_episode) = previous_seed_episode(&item.last_episode, resolved_episode_list) {
        run_ani_cli_continue(item, &seed_episode)
    } else {
        let resolution = resolve_select_nth_for_item_with_diagnostics(item);
        emit_warnings(&resolution.warnings);
        let select_nth = resolution.index.ok_or_else(|| {
            let mut message = "failed to resolve current show for previous action".to_string();
            for warning in resolution.warnings {
                message.push_str("\nWarning: ");
                message.push_str(&warning);
            }
            anyhow!(message)
        })?;
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

#[derive(Debug)]
pub(crate) struct TempHistDir {
    path: PathBuf,
}

impl TempHistDir {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            path: make_temp_hist_dir()?,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn histfile_path(&self) -> PathBuf {
        self.path.join("ani-hsts")
    }
}

impl Drop for TempHistDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
