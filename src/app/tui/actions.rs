use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc;

use anyhow::{Context, Result, anyhow};
use ratatui::widgets::TableState;

use crate::db::{Database, SeenEntry};

use super::super::episode::{
    EpisodeLabelFetchOutcome, fetch_episode_labels_with_diagnostics, parse_episode_f64,
    parse_title_and_total_eps,
};
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
            // A user can activate Select before the background metadata request
            // finishes. Resolve it synchronously while the TUI is suspended so
            // the action does not fail just because the list was still loading.
            let episodes = select_episode_labels(item, episode_list, |ani_id, total_hint| {
                fetch_episode_labels_with_diagnostics(ani_id, total_hint)
            })?;
            let Some(episode) = prompt_episode_selection(&episodes)? else {
                return Ok(format!("Select canceled: {}", item.title));
            };
            let outcome = run_ani_cli_select(item, &episode, Some(&episodes), options)?;
            apply_outcome(db, item, outcome, |ep| {
                format!("Select finished: {} now on episode {ep}", item.title)
            })
        }
    }
}

fn episode_total_hint(item: &SeenEntry) -> Option<u32> {
    parse_title_and_total_eps(&item.title).1.or_else(|| {
        // `total_episodes` is also raised to the watched episode, so an equal
        // value is not evidence that the show ends there. Only use cached
        // metadata as a numeric fallback when it extends beyond progress.
        item.total_episodes.filter(|total| {
            parse_episode_f64(&item.last_episode)
                .is_some_and(|last_episode| last_episode < f64::from(*total))
        })
    })
}

fn select_episode_labels(
    item: &SeenEntry,
    episode_list: Option<&[String]>,
    fetch: impl FnOnce(&str, Option<u32>) -> EpisodeLabelFetchOutcome,
) -> Result<Vec<String>> {
    if let Some(episodes) = episode_list {
        return Ok(episodes.to_vec());
    }

    let outcome = fetch(&item.ani_id, episode_total_hint(item));
    outcome.episode_list.ok_or_else(|| {
        let detail = if outcome.warnings.is_empty() {
            "no episode metadata was returned".to_string()
        } else {
            outcome.warnings.join(" | ")
        };
        anyhow!("episode list unavailable: {detail}")
    })
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
    let total_hint = episode_total_hint(item);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_fetches_episode_labels_when_background_list_is_absent() {
        let item = SeenEntry {
            ani_id: "classroom-of-the-elite-1006".to_string(),
            title: "Classroom of the Elite".to_string(),
            last_episode: "2".to_string(),
            last_seen_at: "2026-08-09T00:00:00+00:00".to_string(),
            total_episodes: Some(2),
            episodes_updated_at: None,
        };

        let episodes = select_episode_labels(&item, None, |ani_id, total_hint| {
            assert_eq!(ani_id, "classroom-of-the-elite-1006");
            assert_eq!(total_hint, None);
            EpisodeLabelFetchOutcome {
                episode_list: Some((1..=12).map(|episode| episode.to_string()).collect()),
                warnings: Vec::new(),
            }
        })
        .expect("Select should synchronously fetch a missing episode list");

        assert_eq!(episodes.len(), 12);
        assert_eq!(episodes.last().map(String::as_str), Some("12"));
    }
}
