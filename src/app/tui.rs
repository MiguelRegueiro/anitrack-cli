use std::collections::HashMap;
use std::io;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Gauge, Padding, Paragraph, Row, Table, TableState,
    Wrap,
};
use ratatui::{Frame, Terminal};

use crate::db::{Database, SeenEntry};

use super::episode::{
    build_progress_gauge, fetch_episode_labels, format_episode_progress_text,
    format_last_seen_display, has_next_episode, has_previous_episode, parse_title_and_total_eps,
    truncate,
};
use super::tracking::{
    run_ani_cli_continue, run_ani_cli_previous, run_ani_cli_replay, run_ani_cli_search,
    run_ani_cli_select,
};

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
struct PendingDelete {
    ani_id: String,
    title: String,
}

#[derive(Debug, Clone)]
struct PendingNotice {
    message: String,
}

#[derive(Debug, Clone)]
struct EpisodeListFetchResult {
    ani_id: String,
    episode_list: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
enum EpisodeListState {
    Loading,
    Ready(Option<Vec<String>>),
}

impl EpisodeListState {
    fn episode_list(&self) -> Option<&[String]> {
        match self {
            Self::Ready(Some(episodes)) => Some(episodes.as_slice()),
            Self::Loading | Self::Ready(None) => None,
        }
    }

    fn is_loading(&self) -> bool {
        matches!(self, Self::Loading)
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
                        status = status_error(&format!("Action failed for {selected_title}: {err}"))
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

#[allow(clippy::too_many_arguments)]
fn draw_tui(
    frame: &mut Frame,
    items: &[SeenEntry],
    table_state: &mut TableState,
    action: TuiAction,
    status: &str,
    pending_delete: Option<&PendingDelete>,
    pending_notice: Option<&PendingNotice>,
    episode_lists_by_id: &HashMap<String, EpisodeListState>,
) {
    let bg = Block::default().style(Style::default().bg(Color::Black));
    frame.render_widget(bg, frame.area());

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let selected_idx = table_state.selected().map(|i| i + 1).unwrap_or(0);
    let selected_text = if selected_idx == 0 {
        "-".to_string()
    } else {
        selected_idx.to_string()
    };
    let mode_text = action.label();
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            "ANITRACK",
            Style::default()
                .fg(Color::Rgb(110, 170, 255))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("   ", Style::default()),
        Span::styled(
            format!("{} entries", items.len()),
            Style::default().fg(Color::Rgb(185, 195, 210)),
        ),
        Span::styled("   ", Style::default()),
        Span::styled(
            format!("selected {selected_text}"),
            Style::default().fg(Color::Rgb(185, 195, 210)),
        ),
        Span::styled("   ", Style::default()),
        Span::styled(mode_text, Style::default().fg(Color::Yellow)),
    ]))
    .alignment(Alignment::Center)
    .block(panel_block("Dashboard"));
    frame.render_widget(header, chunks[0]);

    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(64), Constraint::Percentage(36)])
        .split(chunks[1]);
    let details_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(3)])
        .split(body_chunks[1]);

    let rows: Vec<Row> = items
        .iter()
        .map(|item| {
            let (display_title, total_eps) = parse_title_and_total_eps(&item.title);
            Row::new(vec![
                Cell::from(display_title),
                Cell::from(
                    total_eps
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                ),
                Cell::from(item.last_episode.clone()),
                Cell::from(format_last_seen_display(&item.last_seen_at)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(46),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(33),
        ],
    )
    .header(
        Row::new(vec!["Title", "Total Eps", "Last Ep", "Last Seen"]).style(
            Style::default()
                .fg(Color::Rgb(110, 170, 255))
                .add_modifier(Modifier::BOLD),
        ),
    )
    .block(panel_block("Library"))
    .row_highlight_style(
        Style::default()
            .bg(Color::Rgb(110, 170, 255))
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▸ ");
    frame.render_stateful_widget(table, body_chunks[0], table_state);

    let (selection_text, gauge) = match table_state.selected().and_then(|idx| items.get(idx)) {
        Some(item) => {
            let (title, total_eps) = parse_title_and_total_eps(&item.title);
            let total_eps_text = total_eps
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".to_string());
            let episode_state = episode_lists_by_id.get(&item.ani_id);
            let episode_list = episode_state.and_then(EpisodeListState::episode_list);
            let episode_progress_text = total_eps
                .map(|total| format_episode_progress_text(&item.last_episode, total, episode_list))
                .unwrap_or_else(|| format!("{} of {}", item.last_episode, total_eps_text));
            let gauge = total_eps
                .and_then(|total| build_progress_gauge(&item.last_episode, total, episode_list));
            let mut selection_text = format!(
                "Title\n{}\n\nEpisode\n{}\n\nAni ID\n{}\n\nLast Seen\n{}",
                truncate(&title, 40),
                episode_progress_text,
                truncate(&item.ani_id, 28),
                format_last_seen_display(&item.last_seen_at),
            );
            if episode_state.is_some_and(EpisodeListState::is_loading) {
                selection_text.push_str("\n\nEpisodes\nLoading...");
            }
            (selection_text, gauge)
        }
        None => (
            "No tracked entries yet.\n\nPress s to run ani-cli search and add entries.".to_string(),
            None,
        ),
    };
    let selection = Paragraph::new(selection_text)
        .style(Style::default().fg(Color::Rgb(230, 230, 230)))
        .block(panel_block("Selected"))
        .alignment(Alignment::Left);
    frame.render_widget(selection, details_chunks[0]);
    if let Some((ratio, label)) = gauge {
        let progress = Gauge::default()
            .block(panel_block("Progress"))
            .gauge_style(
                Style::default()
                    .fg(Color::Rgb(130, 190, 255))
                    .bg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            )
            .label(label)
            .ratio(ratio);
        frame.render_widget(progress, details_chunks[1]);
    }

    let action_line = action_selector_line(action);
    let command_bar = Paragraph::new(action_line)
        .alignment(Alignment::Center)
        .block(panel_block("Controls"));
    frame.render_widget(command_bar, chunks[2]);

    let status_widget = Paragraph::new(status.to_string())
        .style(status_style(status))
        .block(panel_block("Status"));
    frame.render_widget(status_widget, chunks[3]);

    if let Some(confirm) = pending_delete {
        let popup_text = format!(
            "Delete tracked entry?\n\n{}\n\nThis cannot be undone.\n\n[y / Enter] Delete   [n / Esc] Cancel",
            truncate(&confirm.title, 56)
        );
        let popup_area = popup_rect_for_text(frame.area(), &popup_text);
        render_popup_shadow(frame, popup_area);
        frame.render_widget(Clear, popup_area);
        let popup = Paragraph::new(popup_text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(modal_block("Confirm Delete"));
        frame.render_widget(popup, popup_area);
    } else if let Some(notice) = pending_notice {
        let popup_area = popup_rect_for_text(frame.area(), &notice.message);
        render_popup_shadow(frame, popup_area);
        frame.render_widget(Clear, popup_area);
        let popup = Paragraph::new(notice.message.clone())
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(modal_block("No More Episodes"));
        frame.render_widget(popup, popup_area);
    }
}

fn panel_block(title: &'static str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(125, 135, 150)))
        .title(title)
}

fn modal_block(title: &'static str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(
            Style::default()
                .fg(Color::Rgb(160, 190, 235))
                .add_modifier(Modifier::BOLD),
        )
        .title(title)
        .padding(Padding::new(2, 2, 1, 1))
}

fn pill_active() -> Style {
    Style::default()
        .bg(Color::Rgb(110, 170, 255))
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD)
}

fn pill_inactive() -> Style {
    Style::default()
        .bg(Color::Rgb(72, 82, 96))
        .fg(Color::Rgb(230, 235, 242))
}

fn action_pill_style(action: TuiAction, current: TuiAction) -> Style {
    if action == current {
        pill_active()
    } else {
        pill_inactive()
    }
}

fn action_selector_line(current: TuiAction) -> Line<'static> {
    Line::from(vec![
        Span::styled(" NEXT ", action_pill_style(TuiAction::Next, current)),
        Span::styled(" ", Style::default()),
        Span::styled(" REPLAY ", action_pill_style(TuiAction::Replay, current)),
        Span::styled(" ", Style::default()),
        Span::styled(
            " PREVIOUS ",
            action_pill_style(TuiAction::Previous, current),
        ),
        Span::styled(" ", Style::default()),
        Span::styled(" SELECT ", action_pill_style(TuiAction::Select, current)),
        Span::styled(
            "   ↑/↓ move  ←/→ action  Enter run  s search  d delete  q quit",
            Style::default().fg(Color::Rgb(185, 195, 210)),
        ),
    ])
}

fn status_style(status: &str) -> Style {
    if status.starts_with("ERROR:") {
        Style::default()
            .fg(Color::Rgb(255, 145, 120))
            .add_modifier(Modifier::BOLD)
    } else if status.starts_with("INFO:") {
        Style::default().fg(Color::Rgb(205, 165, 255))
    } else {
        Style::default().fg(Color::Rgb(230, 235, 242))
    }
}

fn centered_fixed_rect(width: u16, height: u16, area: Rect) -> Rect {
    let clamped_width = width.min(area.width.max(1));
    let clamped_height = height.min(area.height.max(1));
    let x = area.x + area.width.saturating_sub(clamped_width) / 2;
    let y = area.y + area.height.saturating_sub(clamped_height) / 2;
    Rect::new(x, y, clamped_width, clamped_height)
}

fn render_popup_shadow(frame: &mut Frame, popup_area: Rect) {
    let area = frame.area();
    let shadow = Rect::new(
        (popup_area.x + 1).min(area.x + area.width.saturating_sub(1)),
        (popup_area.y + 1).min(area.y + area.height.saturating_sub(1)),
        popup_area.width.saturating_sub(1),
        popup_area.height.saturating_sub(1),
    );
    if shadow.width == 0 || shadow.height == 0 {
        return;
    }
    let shadow_block = Block::default().style(Style::default().bg(Color::Rgb(14, 16, 24)));
    frame.render_widget(shadow_block, shadow);
}

fn popup_rect_for_text(area: Rect, text: &str) -> Rect {
    let max_line_width = text
        .lines()
        .map(|line| line.chars().count() as u16)
        .max()
        .unwrap_or(0);
    let line_count = text.lines().count() as u16;

    let available_width = area.width.saturating_sub(2).max(1);
    let min_width = 48.min(available_width);
    let max_width = 72.min(available_width);
    let desired_width = max_line_width.saturating_add(12);
    let width = desired_width.clamp(min_width, max_width);

    let available_height = area.height.saturating_sub(2).max(1);
    let min_height = 10.min(available_height);
    let max_height = 18.min(available_height);
    let desired_height = line_count.saturating_add(6);
    let height = desired_height.clamp(min_height, max_height);

    centered_fixed_rect(width, height, area)
}

fn refresh_items(
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

fn status_info(msg: &str) -> String {
    format!("INFO: {msg}")
}

fn status_error(msg: &str) -> String {
    format!("ERROR: {msg}")
}

fn run_selected_action(
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

fn ensure_selected_episode_list(
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
        let episode_list = fetch_episode_labels(&ani_id, total_hint);
        let _ = tx.send(EpisodeListFetchResult {
            ani_id,
            episode_list,
        });
    });
}

fn drain_episode_fetch_results(
    rx: &mpsc::Receiver<EpisodeListFetchResult>,
    episode_lists_by_id: &mut HashMap<String, EpisodeListState>,
) {
    while let Ok(result) = rx.try_recv() {
        episode_lists_by_id.insert(result.ani_id, EpisodeListState::Ready(result.episode_list));
    }
}

struct TuiSession {
    active: bool,
}

impl TuiSession {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("failed to enable raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen).context("failed to enter alternate screen")?;
        Ok(Self { active: true })
    }

    fn suspend(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        disable_raw_mode().context("failed to disable raw mode")?;
        execute!(io::stdout(), LeaveAlternateScreen).context("failed to leave alternate screen")?;
        self.active = false;
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        if self.active {
            return Ok(());
        }
        execute!(io::stdout(), EnterAlternateScreen)
            .context("failed to re-enter alternate screen")?;
        enable_raw_mode().context("failed to re-enable raw mode")?;
        self.active = true;
        Ok(())
    }

    fn leave(&mut self) -> Result<()> {
        self.suspend()
    }
}

impl Drop for TuiSession {
    fn drop(&mut self) {
        if self.active {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
        }
    }
}
