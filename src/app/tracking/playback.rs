use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

use super::super::episode::{
    fetch_episode_labels_with_diagnostics, next_target_episode, parse_title_and_total_eps,
    previous_seed_episode, previous_target_episode, replay_seed_episode, sanitize_title_for_search,
};
use super::api::{normalize_title_for_match, resolve_select_nth_for_item_with_diagnostics};
use super::history::{
    ani_cli_histfile, append_history_warnings, detect_latest_watch_event,
    detect_latest_watch_event_from_logs_with_diagnostics, history_file_touched, read_hist_map,
    read_histfile_sig, unix_now_ns,
};
use super::process::{run_interactive_cmd, with_sigint_ignored};
use super::{PlaybackOptions, PlaybackOutcome, ReplayPlan};
use crate::db::{Database, SeenEntry};

fn emit_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("Warning: {warning}");
    }
}

fn playback_failure_detail(status: &ExitStatus) -> String {
    let base = if let Some(code) = status.code() {
        format!("ani-cli exited with code {code}")
    } else {
        #[cfg(unix)]
        {
            if let Some(signal) = status.signal() {
                format!("ani-cli terminated by signal {signal}")
            } else {
                format!("ani-cli exited with status {status}")
            }
        }
        #[cfg(not(unix))]
        {
            format!("ani-cli exited with status {status}")
        }
    };

    if status.code() == Some(1) {
        format!("{base}; possible network outage or interrupted playback")
    } else {
        base
    }
}

fn append_mode_args(cmd: &mut ProcessCommand, options: PlaybackOptions) {
    if options.dub {
        cmd.arg("--dub");
    }
    if options.vlc {
        cmd.arg("--vlc");
    }
}

pub(crate) fn run_ani_cli_search(
    db: &Database,
    options: PlaybackOptions,
) -> Result<(String, Option<String>)> {
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
        append_mode_args(&mut cmd, options);
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

pub(crate) fn ani_cli_major_version(bin: &Path) -> Option<u32> {
    let output = ProcessCommand::new(bin).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .find_map(|part| {
            part.trim_start_matches(|ch: char| !ch.is_ascii_digit())
                .split('.')
                .next()?
                .parse::<u32>()
                .ok()
        })
}

fn uses_new_ani_cli_history(bin: &Path) -> bool {
    ani_cli_major_version(bin).is_some_and(|major| major >= 5)
}

fn is_new_ani_cli_history_id(id: &str) -> bool {
    // ani-cli 5 search results use a slug followed by the numeric source ID.
    id.rsplit_once('-').is_some_and(|(slug, numeric_id)| {
        !slug.is_empty()
            && !numeric_id.is_empty()
            && numeric_id.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn titles_match(left: &str, right: &str) -> bool {
    normalize_title_for_match(left) == normalize_title_for_match(right)
}

fn matching_new_history_entry(item: &SeenEntry) -> Option<super::HistEntry> {
    let history = read_hist_map(&ani_cli_histfile());
    history.ordered_entries.into_iter().rev().find(|entry| {
        is_new_ani_cli_history_id(&entry.id) && titles_match(&entry.title, &item.title)
    })
}

fn runtime_select_nth(item: &SeenEntry) -> Option<u32> {
    if uses_new_ani_cli_history(&resolve_ani_cli_bin()) {
        // The resolver uses ani-cli 4's AllAnime source. ani-cli 5 has a different
        // result set, so forwarding that index could silently select another show.
        return None;
    }
    let resolution = resolve_select_nth_for_item_with_diagnostics(item);
    emit_warnings(&resolution.warnings);
    resolution.index
}

pub(crate) fn run_ani_cli_continue(
    item: &SeenEntry,
    stored_episode: &str,
    options: PlaybackOptions,
) -> Result<PlaybackOutcome> {
    run_ani_cli_continue_to(item, stored_episode, None, options)
}

fn run_ani_cli_continue_to(
    item: &SeenEntry,
    stored_episode: &str,
    target_episode: Option<&str>,
    options: PlaybackOptions,
) -> Result<PlaybackOutcome> {
    let ani_cli_bin = resolve_ani_cli_bin();
    if uses_new_ani_cli_history(&ani_cli_bin) && !is_new_ani_cli_history_id(&item.ani_id) {
        if let Some(current) = matching_new_history_entry(item) {
            return run_ani_cli_continue_seeded(
                &ani_cli_bin,
                &current.id,
                &current.title,
                stored_episode,
                options,
            );
        }

        // A legacy AllAnime ID cannot be continued by ani-cli 5. Search by the
        // cleaned title and request the intended episode; ani-cli will then write
        // its current ID to the normal history for subsequent actions.
        let derived_target;
        let target_episode = if let Some(target_episode) = target_episode {
            target_episode
        } else {
            derived_target = next_target_episode(stored_episode, None).ok_or_else(|| {
                anyhow!(
                    "cannot determine the episode after {stored_episode:?} for {}",
                    item.title
                )
            })?;
            &derived_target
        };
        return run_ani_cli_episode_with_global_tracking(item, target_episode, None, options);
    }

    run_ani_cli_continue_seeded(
        &ani_cli_bin,
        &item.ani_id,
        &item.title,
        stored_episode,
        options,
    )
}

fn run_ani_cli_continue_seeded(
    ani_cli_bin: &Path,
    ani_id: &str,
    title: &str,
    stored_episode: &str,
    options: PlaybackOptions,
) -> Result<PlaybackOutcome> {
    let temp_hist_dir = TempHistDir::new()?;
    let histfile = temp_hist_dir.histfile_path();
    fs::write(&histfile, format!("{stored_episode}\t{ani_id}\t{title}\n")).with_context(|| {
        format!(
            "failed writing temp ani-cli history at {}",
            histfile.display()
        )
    })?;

    // Use plain .status() rather than run_interactive_cmd: ani-cli -c operates non-interactively
    // using the seeded temp history to skip the search prompt, so TTY foreground transfer is not needed.
    let mut cmd = ProcessCommand::new(ani_cli_bin);
    append_mode_args(&mut cmd, options);
    let status = cmd
        .arg("-c")
        .env("ANI_CLI_HIST_DIR", temp_hist_dir.path())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to launch {}", ani_cli_bin.display()))?;
    let success = status.success();
    let final_episode = if success {
        let hist_read = read_hist_map(&histfile);
        emit_warnings(&hist_read.warnings);
        hist_read.entries.get(ani_id).map(|entry| entry.ep.clone())
    } else {
        None
    };

    Ok(PlaybackOutcome {
        success,
        final_episode,
        failure_detail: (!success).then(|| playback_failure_detail(&status)),
    })
}

pub(crate) fn run_ani_cli_episode(
    title: &str,
    select_nth: Option<u32>,
    episode: &str,
    options: PlaybackOptions,
) -> Result<ExitStatus> {
    let ani_cli_bin = resolve_ani_cli_bin();
    let mut cmd = ProcessCommand::new(&ani_cli_bin);
    append_mode_args(&mut cmd, options);
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
    Ok(status)
}

fn run_with_global_tracking(
    item: &SeenEntry,
    requested_episode: &str,
    run_cmd: impl FnOnce() -> Result<ExitStatus>,
) -> Result<PlaybackOutcome> {
    let histfile = ani_cli_histfile();
    let before_read = read_hist_map(&histfile);
    emit_warnings(&before_read.warnings);
    let before = before_read.entries;
    let before_ordered = before_read.ordered_entries;

    let status = run_cmd()?;
    let success = status.success();
    let final_episode = if success {
        let after_read = read_hist_map(&histfile);
        emit_warnings(&after_read.warnings);
        let changed =
            detect_latest_watch_event(&before, &before_ordered, &after_read.ordered_entries);
        changed
            .map(|entry| entry.ep)
            .or_else(|| {
                after_read
                    .entries
                    .get(&item.ani_id)
                    .map(|entry| entry.ep.clone())
            })
            .or_else(|| {
                after_read
                    .ordered_entries
                    .iter()
                    .rev()
                    .find(|entry| titles_match(&entry.title, &item.title))
                    .map(|entry| entry.ep.clone())
            })
            .or_else(|| Some(requested_episode.to_string()))
    } else {
        None
    };

    Ok(PlaybackOutcome {
        success,
        final_episode,
        failure_detail: (!success).then(|| playback_failure_detail(&status)),
    })
}

pub(crate) fn run_ani_cli_episode_with_global_tracking(
    item: &SeenEntry,
    episode: &str,
    select_nth: Option<u32>,
    options: PlaybackOptions,
) -> Result<PlaybackOutcome> {
    let title = sanitize_title_for_search(&item.title);
    run_with_global_tracking(item, episode, || {
        run_ani_cli_episode(&title, select_nth, episode, options)
    })
}

pub(crate) fn run_ani_cli_select(
    item: &SeenEntry,
    episode: &str,
    episode_list: Option<&[String]>,
    options: PlaybackOptions,
) -> Result<PlaybackOutcome> {
    if let Some(seed_episode) = replay_seed_episode(episode, episode_list) {
        return run_ani_cli_continue_to(item, &seed_episode, Some(episode), options);
    }

    let select_nth = runtime_select_nth(item);
    run_ani_cli_episode_with_global_tracking(item, episode, select_nth, options)
}

pub(crate) fn run_ani_cli_replay(
    item: &SeenEntry,
    episode_list: Option<&[String]>,
    options: PlaybackOptions,
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

    let plan = build_replay_plan(item, resolved_episode_list, runtime_select_nth);
    match plan {
        ReplayPlan::Continue { seed_episode } => {
            run_ani_cli_continue_to(item, &seed_episode, Some(&item.last_episode), options)
        }
        ReplayPlan::Episode {
            episode,
            select_nth,
        } => run_ani_cli_episode_with_global_tracking(item, &episode, select_nth, options),
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
    options: PlaybackOptions,
) -> Result<PlaybackOutcome> {
    let fetched_episodes = if episode_list.is_none() {
        let total_hint = parse_title_and_total_eps(&item.title).1;
        let outcome = fetch_episode_labels_with_diagnostics(&item.ani_id, total_hint);
        emit_warnings(&outcome.warnings);
        outcome.episode_list
    } else {
        None
    };
    let resolved_episode_list = episode_list.or(fetched_episodes.as_deref());

    let target_episode = previous_target_episode(&item.last_episode, resolved_episode_list)
        .ok_or_else(|| anyhow!("no previous episode available"))?;
    if let Some(seed_episode) = previous_seed_episode(&item.last_episode, resolved_episode_list) {
        run_ani_cli_continue_to(item, &seed_episode, Some(&target_episode), options)
    } else {
        let select_nth = runtime_select_nth(item);
        if !uses_new_ani_cli_history(&resolve_ani_cli_bin()) && select_nth.is_none() {
            return Err(anyhow!(
                "failed to resolve current show for previous action"
            ));
        }
        run_ani_cli_episode_with_global_tracking(item, &target_episode, select_nth, options)
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
