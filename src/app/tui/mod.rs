mod actions;
mod render;
mod session;

use std::collections::HashMap;
use std::io;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::TableState;

use crate::db::Database;

use super::episode::{has_next_episode, has_previous_episode, parse_title_and_total_eps, truncate};
use super::tracking::run_ani_cli_search;

use self::actions::{
    drain_episode_fetch_results, ensure_selected_episode_list, refresh_items, run_selected_action,
    status_error, status_info,
};
use self::render::draw_tui;
use self::session::TuiSession;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TuiAction {
    Next,
    Replay,
    Previous,
    Select,
}

impl TuiAction {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Next => "NEXT",
            Self::Replay => "REPLAY",
            Self::Previous => "PREVIOUS",
            Self::Select => "SELECT",
        }
    }

    pub(crate) fn move_left(self) -> Self {
        match self {
            Self::Next => Self::Next,
            Self::Replay => Self::Next,
            Self::Previous => Self::Replay,
            Self::Select => Self::Previous,
        }
    }

    pub(crate) fn move_right(self) -> Self {
        match self {
            Self::Next => Self::Replay,
            Self::Replay => Self::Previous,
            Self::Previous => Self::Select,
            Self::Select => Self::Select,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct PendingDelete {
    pub(super) ani_id: String,
    pub(super) title: String,
}

#[derive(Debug, Clone)]
pub(super) struct PendingNotice {
    pub(super) message: String,
}

#[derive(Debug, Clone)]
pub(super) struct EpisodeListFetchResult {
    pub(super) ani_id: String,
    pub(super) episode_list: Option<Vec<String>>,
    pub(super) warning: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) enum EpisodeListState {
    Loading,
    Ready {
        episode_list: Option<Vec<String>>,
        warning: Option<String>,
    },
}

impl EpisodeListState {
    pub(super) fn episode_list(&self) -> Option<&[String]> {
        match self {
            Self::Ready {
                episode_list: Some(episodes),
                ..
            } => Some(episodes.as_slice()),
            Self::Loading
            | Self::Ready {
                episode_list: None, ..
            } => None,
        }
    }

    pub(super) fn is_loading(&self) -> bool {
        matches!(self, Self::Loading)
    }

    pub(super) fn warning(&self) -> Option<&str> {
        match self {
            Self::Ready {
                warning: Some(warning),
                ..
            } => Some(warning.as_str()),
            _ => None,
        }
    }
}

pub(crate) fn run_tui(db: &Database) -> Result<()> {
    let mut session = TuiSession::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .context("failed to initialize terminal backend")?;
    terminal.clear()?;

    let mut items = db.list_seen()?;
    let mut table_state = TableState::default();
    table_state.select((!items.is_empty()).then_some(0));
    let mut action = TuiAction::Next;
    let mut pending_delete = None::<PendingDelete>;
    let mut pending_notice = None::<PendingNotice>;
    let mut episode_lists_by_id: HashMap<String, EpisodeListState> = HashMap::new();
    let (episode_fetch_tx, episode_fetch_rx) = mpsc::channel::<EpisodeListFetchResult>();
    let mut status = if items.is_empty() {
        status_info("No tracked entries yet. Press `s` to search or run `anitrack start`.")
    } else {
        status_info("Ready.")
    };

    loop {
        drain_episode_fetch_results(&episode_fetch_rx, &mut episode_lists_by_id);
        ensure_selected_episode_list(
            &items,
            &table_state,
            &mut episode_lists_by_id,
            &episode_fetch_tx,
        );
        terminal.draw(|frame| {
            draw_tui(
                frame,
                &items,
                &mut table_state,
                action,
                &status,
                pending_delete.as_ref(),
                pending_notice.as_ref(),
                &episode_lists_by_id,
            )
        })?;

        if !event::poll(Duration::from_millis(200))? {
            continue;
        }

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if pending_notice.is_some() {
            pending_notice = None;
            continue;
        }

        if let Some(dialog) = pending_delete.as_ref() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    let deleting_id = dialog.ani_id.clone();
                    let deleting_title = dialog.title.clone();
                    pending_delete = None;
                    match db.delete_seen(&deleting_id) {
                        Ok(true) => {
                            status =
                                status_info(&format!("Deleted tracked entry: {deleting_title}"));
                            refresh_items(db, &mut items, &mut table_state, None)?;
                        }
                        Ok(false) => {
                            status = status_error("Delete failed: entry no longer exists.");
                            refresh_items(db, &mut items, &mut table_state, None)?;
                        }
                        Err(err) => status = status_error(&format!("Delete failed: {err}")),
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') => {
                    pending_delete = None;
                    status = status_info("Delete canceled.");
                }
                _ => {}
            }
            continue;
        }

        match key.code {
            KeyCode::Char('q') => break,
            KeyCode::Char('s') => {
                session.suspend()?;
                let result = run_ani_cli_search(db);
                session.resume()?;
                terminal.clear()?;

                match result {
                    Ok((msg, changed_id)) => {
                        status = status_info(&msg);
                        refresh_items(db, &mut items, &mut table_state, changed_id.as_deref())?;
                    }
                    Err(err) => status = status_error(&format!("Search failed: {err}")),
                }
            }
            KeyCode::Up => {
                if let Some(selected) = table_state.selected() {
                    table_state.select(Some(selected.saturating_sub(1)));
                }
            }
            KeyCode::Down => {
                if let Some(selected) = table_state.selected()
                    && !items.is_empty()
                {
                    let next = (selected + 1).min(items.len().saturating_sub(1));
                    table_state.select(Some(next));
                }
            }
            KeyCode::Left => action = action.move_left(),
            KeyCode::Right => action = action.move_right(),
            KeyCode::Char('d') => {
                let Some(selected) = table_state.selected() else {
                    status = status_error("Delete failed: no entry selected.");
                    continue;
                };
                if selected >= items.len() {
                    status = status_error("Delete failed: invalid selection.");
                    continue;
                }
                let selected_item = &items[selected];
                pending_delete = Some(PendingDelete {
                    ani_id: selected_item.ani_id.clone(),
                    title: selected_item.title.clone(),
                });
                status = status_info("Confirm delete: y/Enter to delete, n/Esc to cancel.");
            }
            KeyCode::Enter => {
                let Some(selected) = table_state.selected() else {
                    continue;
                };
                if selected >= items.len() {
                    continue;
                }
                let selected_item = &items[selected];
                let episode_list = episode_lists_by_id
                    .get(&selected_item.ani_id)
                    .and_then(EpisodeListState::episode_list);

                if matches!(action, TuiAction::Next) {
                    let total_eps = parse_title_and_total_eps(&selected_item.title).1;
                    if !has_next_episode(&selected_item.last_episode, total_eps, episode_list) {
                        pending_notice = Some(PendingNotice {
                            message: format!(
                                "No more episodes available.\n\n{}\n\nPress any key to continue.",
                                truncate(&selected_item.title, 50)
                            ),
                        });
                        status = status_info("No next episode available.");
                        continue;
                    }
                }

                if matches!(action, TuiAction::Previous)
                    && !has_previous_episode(&selected_item.last_episode, episode_list)
                {
                    pending_notice = Some(PendingNotice {
                        message: format!(
                            "No previous episode available.\n\n{}\n\nPress any key to continue.",
                            truncate(&selected_item.title, 50)
                        ),
                    });
                    status = status_info("No previous episode available.");
                    continue;
                }

                let selected_id = items[selected].ani_id.clone();
                let selected_title = items[selected].title.clone();

                session.suspend()?;
                let result = run_selected_action(db, &items[selected], action, episode_list);
                session.resume()?;
                terminal.clear()?;

                match result {
                    Ok(msg) => status = status_info(&msg),
                    Err(err) => {
                        let no_previous = matches!(action, TuiAction::Previous)
                            && err.chain().any(|cause| {
                                cause.to_string().contains("no previous episode available")
                            });
                        if no_previous {
                            pending_notice = Some(PendingNotice {
                                message: format!(
                                    "No previous episode available.\n\n{}\n\nPress any key to continue.",
                                    truncate(&selected_title, 50)
                                ),
                            });
                            status = status_info("No previous episode available.");
                        } else {
                            status =
                                status_error(&format!("Action failed for {selected_title}: {err}"));
                        }
                    }
                }

                refresh_items(db, &mut items, &mut table_state, Some(&selected_id))?;
            }
            _ => {}
        }
    }

    terminal.show_cursor()?;
    session.leave()?;
    Ok(())
}
