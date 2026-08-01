use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc;

use anyhow::{Context, Result, anyhow};
use ratatui::widgets::TableState;

use crate::db::{Database, SeenEntry};

use super::super::episode::{fetch_episode_labels_with_diagnostics, parse_title_and_total_eps};
use super::super::tracking::{
    PlaybackOptions, PlaybackOutcome, playback_failure_message, run_ani_cli_continue,
    run_ani_cli_previous, run_ani_cli_replay, run_ani_cli_select,
};
use super::{EpisodeListFetchResult, EpisodeListState, TuiAction};

pub(super) fn refresh_items(
    db: &Database,
    items: &mut Vec<SeenEntry>,
    table_state: &mut TableState,
    preferred_id: Option<&str>,
) -> Result<()> {
    *items = db.list_seen()?;
    if items.is_empty() {
        table_state.select(None);
        return Ok(());
    }

    if let Some(id) = preferred_id
        && let Some(idx) = items.iter().position(|item| item.ani_id == id)
    {
        table_state.select(Some(idx));
        return Ok(());
    }

    match table_state.selected() {
        Some(selected) => table_state.select(Some(selected.min(items.len() - 1))),
        None => table_state.select(Some(0)),
    }
    Ok(())
}

pub(super) fn status_info(msg: &str) -> String {
    format!("INFO: {msg}")
}

pub(super) fn status_error(msg: &str) -> String {
    format!("ERROR: {msg}")
}

fn apply_outcome(
    db: &Database,
    item: &SeenEntry,
    outcome: PlaybackOutcome,
    success_msg: impl FnOnce(&str) -> String,
) -> Result<String> {
    if outcome.success {
        let updated_ep = outcome
            .final_episode
            .unwrap_or_else(|| item.last_episode.clone());
        db.upsert_seen(&item.ani_id, &item.title, &updated_ep)?;
        Ok(success_msg(&updated_ep))
    } else {
        Ok(playback_failure_message(&outcome))
    }
}

pub(super) fn run_selected_action(
    db: &Database,
    item: &SeenEntry,
    action: TuiAction,
    episode_list: Option<&[String]>,
    options: PlaybackOptions,
) -> Result<String> {
    match action {
        TuiAction::Next => {
            let outcome = run_ani_cli_continue(item, &item.last_episode, options)?;
            apply_outcome(db, item, outcome, |ep| {
                format!("Updated progress: {} -> episode {ep}", item.title)
            })
        }
        TuiAction::Replay => {
            let outcome = run_ani_cli_replay(item, episode_list, options)?;
            apply_outcome(db, item, outcome, |ep| {
                format!("Replay finished: {} now on episode {ep}", item.title)
            })
        }
        TuiAction::Previous => {
            let outcome = run_ani_cli_previous(item, episode_list, options)?;
            apply_outcome(db, item, outcome, |ep| {
                format!("Previous finished: {} now on episode {ep}", item.title)
            })
        }
        TuiAction::Select => {
            let episodes =
                episode_list.ok_or_else(|| anyhow!("episode list is still loading/unavailable"))?;
            let Some(episode) = prompt_episode_selection(episodes)? else {
                return Ok(format!("Select canceled: {}", item.title));
            };
            let outcome = run_ani_cli_select(item, &episode, episode_list, options)?;
            apply_outcome(db, item, outcome, |ep| {
                format!("Select finished: {} now on episode {ep}", item.title)
            })
        }
    }
}

fn prompt_episode_selection(episodes: &[String]) -> Result<Option<String>> {
    let mut cmd = Command::new("fzf");
    cmd.arg("--reverse")
        .arg("--cycle")
        .arg("--prompt")
        .arg("Select episode: ")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let mut child = cmd.spawn().context("failed to launch fzf")?;
    {
        let mut stdin = child.stdin.take().context("failed to open fzf stdin")?;
        for episode in episodes {
            writeln!(stdin, "{episode}").context("failed to write episode list to fzf")?;
        }
    }

    let output = child
        .wait_with_output()
        .context("failed waiting for fzf episode selection")?;
    if !output.status.success() {
        return Ok(None);
    }

    let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!selected.is_empty()).then_some(selected))
}

pub(super) fn ensure_selected_episode_list(
    items: &[SeenEntry],
    table_state: &TableState,
    episode_lists_by_id: &mut HashMap<String, EpisodeListState>,
    tx: &mpsc::Sender<EpisodeListFetchResult>,
) {
    let Some(selected_idx) = table_state.selected() else {
        return;
    };
    let Some(item) = items.get(selected_idx) else {
        return;
    };
    if episode_lists_by_id.contains_key(&item.ani_id) {
        return;
    }

    episode_lists_by_id.insert(item.ani_id.clone(), EpisodeListState::Loading);
    let ani_id = item.ani_id.clone();
    let total_hint = parse_title_and_total_eps(&item.title).1;
    let tx = tx.clone();
    std::thread::spawn(move || {
        let outcome = fetch_episode_labels_with_diagnostics(&ani_id, total_hint);
        let warning = (!outcome.warnings.is_empty()).then(|| outcome.warnings.join(" | "));
        let total_episodes = outcome
            .episode_list
            .as_ref()
            .and_then(|episodes| u32::try_from(episodes.len()).ok())
            .filter(|count| *count > 0);
        let _ = tx.send(EpisodeListFetchResult {
            ani_id,
            episode_list: outcome.episode_list,
            total_episodes,
            warning,
        });
    });
}

pub(super) fn drain_episode_fetch_results(
    db: &Database,
    rx: &mpsc::Receiver<EpisodeListFetchResult>,
    episode_lists_by_id: &mut HashMap<String, EpisodeListState>,
) -> Result<bool> {
    let mut metadata_updated = false;
    while let Ok(result) = rx.try_recv() {
        if let Some(total_episodes) = result.total_episodes {
            metadata_updated |= db.update_episode_metadata(&result.ani_id, total_episodes)?;
        }
        episode_lists_by_id.insert(
            result.ani_id,
            EpisodeListState::Ready {
                episode_list: result.episode_list,
                warning: result.warning,
            },
        );
    }
    Ok(metadata_updated)
}
