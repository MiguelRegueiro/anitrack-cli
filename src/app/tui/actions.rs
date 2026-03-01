use std::collections::HashMap;
use std::sync::mpsc;

use anyhow::Result;
use ratatui::widgets::TableState;

use crate::db::{Database, SeenEntry};

use super::super::episode::{fetch_episode_labels_with_diagnostics, parse_title_and_total_eps};
use super::super::tracking::{
    run_ani_cli_continue, run_ani_cli_previous, run_ani_cli_replay, run_ani_cli_select,
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

pub(super) fn run_selected_action(
    db: &Database,
    item: &SeenEntry,
    action: TuiAction,
    episode_list: Option<&[String]>,
) -> Result<String> {
    match action {
        TuiAction::Next => {
            let outcome = run_ani_cli_continue(item, &item.last_episode)?;
            if outcome.success {
                let updated_ep = outcome
                    .final_episode
                    .unwrap_or_else(|| item.last_episode.clone());
                db.upsert_seen(&item.ani_id, &item.title, &updated_ep)?;
                Ok(format!(
                    "Updated progress: {} -> episode {}",
                    item.title, updated_ep
                ))
            } else {
                Ok("Playback failed/interrupted. Progress not updated.".to_string())
            }
        }
        TuiAction::Replay => {
            let outcome = run_ani_cli_replay(item, episode_list)?;
            if outcome.success {
                let updated_ep = outcome
                    .final_episode
                    .unwrap_or_else(|| item.last_episode.clone());
                db.upsert_seen(&item.ani_id, &item.title, &updated_ep)?;
                Ok(format!(
                    "Replay finished: {} now on episode {}",
                    item.title, updated_ep
                ))
            } else {
                Ok("Playback failed/interrupted. Progress not updated.".to_string())
            }
        }
        TuiAction::Previous => {
            let outcome = run_ani_cli_previous(item, episode_list)?;
            if outcome.success {
                let updated_ep = outcome
                    .final_episode
                    .unwrap_or_else(|| item.last_episode.clone());
                db.upsert_seen(&item.ani_id, &item.title, &updated_ep)?;
                Ok(format!(
                    "Previous finished: {} now on episode {}",
                    item.title, updated_ep
                ))
            } else {
                Ok("Playback failed/interrupted. Progress not updated.".to_string())
            }
        }
        TuiAction::Select => {
            let outcome = run_ani_cli_select(item)?;
            if outcome.success {
                let updated_ep = outcome
                    .final_episode
                    .unwrap_or_else(|| item.last_episode.clone());
                db.upsert_seen(&item.ani_id, &item.title, &updated_ep)?;
                Ok(format!(
                    "Select finished: {} now on episode {}",
                    item.title, updated_ep
                ))
            } else {
                Ok("Playback failed/interrupted. Progress not updated.".to_string())
            }
        }
    }
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
        let _ = tx.send(EpisodeListFetchResult {
            ani_id,
            episode_list: outcome.episode_list,
            warning,
        });
    });
}

pub(super) fn drain_episode_fetch_results(
    rx: &mpsc::Receiver<EpisodeListFetchResult>,
    episode_lists_by_id: &mut HashMap<String, EpisodeListState>,
) {
    while let Ok(result) = rx.try_recv() {
        episode_lists_by_id.insert(
            result.ani_id,
            EpisodeListState::Ready {
                episode_list: result.episode_list,
                warning: result.warning,
            },
        );
    }
}
