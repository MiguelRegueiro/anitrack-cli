use std::cmp::Ordering;
use std::collections::HashMap;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitStatus, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use chrono::DateTime;
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

use crate::cli::{Cli, Command};
use crate::db::Database;
use crate::paths::database_file_path;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct HistEntry {
    ep: String,
    id: String,
    title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HistFileSig {
    len: u64,
    modified_ns: u128,
}

pub fn run(cli: Cli) -> Result<()> {
    let db = open_db()?;

    match cli.command {
        Some(Command::Start) => run_start(&db)?,
        Some(Command::Next) => run_next(&db)?,
        Some(Command::Replay) => run_replay(&db)?,
        Some(Command::List) => run_list(&db)?,
        Some(Command::Tui) | None => run_tui(&db)?,
    }

    Ok(())
}

fn run_start(db: &Database) -> Result<()> {
    let (message, _) = run_ani_cli_search(db)?;
    println!("\n{message}");
    Ok(())
}

fn run_next(db: &Database) -> Result<()> {
    match db.last_seen()? {
        Some(item) => {
            println!("Playing next episode for last seen show:");
            println!("  Title: {}", item.title);
            println!("  Current stored episode: {}", item.last_episode);

            let outcome = match run_ani_cli_continue(&item, &item.last_episode) {
                Ok(outcome) => outcome,
                Err(err) => {
                    println!("ani-cli launch failed: {err}");
                    println!("Progress not updated.");
                    return Ok(());
                }
            };
            if outcome.success {
                let updated_ep = outcome
                    .final_episode
                    .unwrap_or_else(|| item.last_episode.clone());
                db.upsert_seen(&item.ani_id, &item.title, &updated_ep)?;
                println!("Updated progress: {} -> episode {}", item.title, updated_ep);
            } else {
                println!("Playback failed/interrupted. Progress not updated.");
            }
        }
        None => println!("No last seen entry yet. Run `anitrack start` first."),
    }
    Ok(())
}

fn run_replay(db: &Database) -> Result<()> {
    match db.last_seen()? {
        Some(item) => {
            println!("Replaying last seen episode:");
            println!("  Title: {}", item.title);
            println!("  Episode: {}", item.last_episode);

            let outcome = run_ani_cli_replay(&item);
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(err) => {
                    println!("ani-cli launch failed: {err}");
                    println!("Progress not updated.");
                    return Ok(());
                }
            };
            if outcome.success {
                let updated_ep = outcome
                    .final_episode
                    .unwrap_or_else(|| item.last_episode.clone());
                db.upsert_seen(&item.ani_id, &item.title, &updated_ep)?;
                println!(
                    "Replay finished: {} now on episode {}",
                    item.title, updated_ep
                );
            } else {
                println!("Playback failed/interrupted. Progress not updated.");
            }
        }
        None => println!("No last seen entry yet. Run `anitrack start` first."),
    }
    Ok(())
}

fn run_list(db: &Database) -> Result<()> {
    let items = db.list_seen()?;
    if items.is_empty() {
        println!("No tracked entries yet. Run `anitrack start` first.");
        return Ok(());
    }

    println!(
        "{:<20} {:<40} {:<10} {:<28}",
        "ANI ID", "TITLE", "EP", "LAST SEEN"
    );
    for item in items {
        println!(
            "{:<20} {:<40} {:<10} {:<28}",
            truncate(&item.ani_id, 20),
            truncate(&item.title, 40),
            item.last_episode,
            format_last_seen_display(&item.last_seen_at)
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum TuiAction {
    Next,
    Replay,
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

fn run_tui(db: &Database) -> Result<()> {
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
    let mut episode_lists_by_id: HashMap<String, Option<Vec<String>>> = HashMap::new();
    let mut status = if items.is_empty() {
        status_info("No tracked entries yet. Press `s` to search or run `anitrack start`.")
    } else {
        status_info("Ready.")
    };

    loop {
        ensure_selected_episode_list(&items, &table_state, &mut episode_lists_by_id);
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
            KeyCode::Left => action = TuiAction::Next,
            KeyCode::Right => action = TuiAction::Replay,
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

                if matches!(action, TuiAction::Next) {
                    let total_eps = parse_title_and_total_eps(&selected_item.title).1;
                    let episode_list = episode_lists_by_id
                        .get(&selected_item.ani_id)
                        .and_then(|episodes| episodes.as_ref())
                        .map(|episodes| episodes.as_slice());
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

                let selected_id = items[selected].ani_id.clone();
                let selected_title = items[selected].title.clone();

                session.suspend()?;
                let result = run_selected_action(db, &items[selected], action);
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
    items: &[crate::db::SeenEntry],
    table_state: &mut TableState,
    action: TuiAction,
    status: &str,
    pending_delete: Option<&PendingDelete>,
    pending_notice: Option<&PendingNotice>,
    episode_lists_by_id: &HashMap<String, Option<Vec<String>>>,
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
    let mode_text = match action {
        TuiAction::Next => "NEXT",
        TuiAction::Replay => "REPLAY",
    };
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
            let episode_list = episode_lists_by_id
                .get(&item.ani_id)
                .and_then(|episodes| episodes.as_ref())
                .map(|episodes| episodes.as_slice());
            let episode_progress_text = total_eps
                .map(|total| format_episode_progress_text(&item.last_episode, total, episode_list))
                .unwrap_or_else(|| format!("{} of {}", item.last_episode, total_eps_text));
            let gauge = total_eps.and_then(|total| {
                build_progress_gauge(&item.last_episode, total, episode_list)
            });
            (
                format!(
                    "Title\n{}\n\nEpisode\n{}\n\nAni ID\n{}\n\nLast Seen\n{}",
                    truncate(&title, 40),
                    episode_progress_text,
                    truncate(&item.ani_id, 28),
                    format_last_seen_display(&item.last_seen_at),
                ),
                gauge,
            )
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

    let action_line = match action {
        TuiAction::Next => Line::from(vec![
            Span::styled(" NEXT ", pill_active()),
            Span::styled(" ", Style::default()),
            Span::styled(" REPLAY ", pill_inactive()),
            Span::styled(
                "   ↑/↓ move  ←/→ mode  Enter run  s search  d delete  q quit",
                Style::default().fg(Color::Rgb(185, 195, 210)),
            ),
        ]),
        TuiAction::Replay => Line::from(vec![
            Span::styled(" NEXT ", pill_inactive()),
            Span::styled(" ", Style::default()),
            Span::styled(" REPLAY ", pill_active()),
            Span::styled(
                "   ↑/↓ move  ←/→ mode  Enter run  s search  d delete  q quit",
                Style::default().fg(Color::Rgb(185, 195, 210)),
            ),
        ]),
    };
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
    items: &mut Vec<crate::db::SeenEntry>,
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
    item: &crate::db::SeenEntry,
    action: TuiAction,
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
            let outcome = run_ani_cli_replay(item)?;
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
    }
}

fn parse_title_and_total_eps(title: &str) -> (String, Option<u32>) {
    let trimmed = title.trim();
    let Some(open_idx) = trimmed.rfind('(') else {
        return (trimmed.to_string(), None);
    };
    if !trimmed.ends_with(')') {
        return (trimmed.to_string(), None);
    }
    let inner = trimmed[open_idx + 1..trimmed.len() - 1].trim();
    let Some(num_str) = inner.strip_suffix(" episodes") else {
        return (trimmed.to_string(), None);
    };
    let Ok(num) = num_str.trim().parse::<u32>() else {
        return (trimmed.to_string(), None);
    };
    (trimmed[..open_idx].trim().to_string(), Some(num))
}

fn ensure_selected_episode_list(
    items: &[crate::db::SeenEntry],
    table_state: &TableState,
    episode_lists_by_id: &mut HashMap<String, Option<Vec<String>>>,
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

    let total_hint = parse_title_and_total_eps(&item.title).1;
    let fetched = fetch_episode_labels(&item.ani_id, total_hint);
    episode_lists_by_id.insert(item.ani_id.clone(), fetched);
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

fn open_db() -> Result<Database> {
    let db_path = database_file_path()?;
    let db = Database::open(&db_path)?;
    db.migrate()?;
    Ok(db)
}

#[cfg(unix)]
fn with_sigint_ignored<F, R>(f: F) -> Result<R>
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
fn with_sigint_ignored<F, R>(f: F) -> Result<R>
where
    F: FnOnce() -> Result<R>,
{
    f()
}

#[cfg(unix)]
fn run_interactive_cmd(mut cmd: ProcessCommand) -> Result<ExitStatus> {
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
fn run_interactive_cmd(mut cmd: ProcessCommand) -> Result<ExitStatus> {
    cmd.status().context("failed to launch ani-cli")
}

fn run_ani_cli_search(db: &Database) -> Result<(String, Option<String>)> {
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
    let changed = detect_latest_watch_event(&before, &before_ordered, &after_ordered).or_else(|| {
        detect_latest_watch_event_from_logs(log_window_start_ns, log_window_end_ns, &after_ordered)
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

fn resolve_ani_cli_bin() -> PathBuf {
    PathBuf::from("ani-cli")
}

#[derive(Debug, Clone)]
struct PlaybackOutcome {
    success: bool,
    final_episode: Option<String>,
}

fn run_ani_cli_continue(
    item: &crate::db::SeenEntry,
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

fn run_ani_cli_episode(title: &str, episode: &str) -> Result<bool> {
    let ani_cli_bin = resolve_ani_cli_bin();
    let status = ProcessCommand::new(&ani_cli_bin)
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

fn run_ani_cli_episode_with_global_tracking(
    item: &crate::db::SeenEntry,
    episode: &str,
) -> Result<PlaybackOutcome> {
    let histfile = ani_cli_histfile();
    let before_read = read_hist_map(&histfile);
    for warning in before_read.warnings {
        eprintln!("Warning: {warning}");
    }
    let before = before_read.entries;
    let success = run_ani_cli_episode(&sanitize_title_for_search(&item.title), episode)?;
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

fn run_ani_cli_replay(item: &crate::db::SeenEntry) -> Result<PlaybackOutcome> {
    if let Some(seed_episode) = resolve_replay_seed_episode(item) {
        run_ani_cli_continue(item, &seed_episode)
    } else {
        run_ani_cli_episode_with_global_tracking(item, &item.last_episode)
    }
}

fn make_temp_hist_dir() -> Result<PathBuf> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = env::temp_dir().join(format!("anitrack-hist-{}-{ts}", std::process::id()));
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create temp history dir {}", dir.display()))?;
    Ok(dir)
}

fn ani_cli_histfile() -> PathBuf {
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

fn parse_hist_map(raw: &str) -> (HashMap<String, HistEntry>, Vec<HistEntry>, usize) {
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

fn parse_hist_line(line: &str) -> Option<HistEntry> {
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

fn append_history_warnings(message: &mut String, warnings: &[String]) {
    for warning in warnings {
        message.push_str("\nWarning: ");
        message.push_str(warning);
    }
}

fn detect_changed_latest(
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

fn added_entries(before_ordered: &[HistEntry], after_ordered: &[HistEntry]) -> Vec<HistEntry> {
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

fn detect_latest_added_entry(
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

fn detect_latest_watch_event(
    before: &HashMap<String, HistEntry>,
    before_ordered: &[HistEntry],
    after_ordered: &[HistEntry],
) -> Option<HistEntry> {
    detect_latest_added_entry(before, before_ordered, after_ordered)
        .or_else(|| detect_changed_latest(before, after_ordered))
}

fn read_histfile_sig(path: &Path) -> Option<HistFileSig> {
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

fn history_file_touched(before: Option<HistFileSig>, after: Option<HistFileSig>) -> bool {
    before != after
}

fn unix_now_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn parse_short_unix_ts_ns(raw: &str) -> Option<u128> {
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

fn parse_journal_ani_cli_line(line: &str) -> Option<(u128, String)> {
    let (ts_raw, rest) = line.split_once(' ')?;
    let ts_ns = parse_short_unix_ts_ns(ts_raw)?;
    let (_, msg) = rest.split_once(": ")?;
    Some((ts_ns, msg.trim().to_string()))
}

fn ani_cli_log_key(title: &str, episode: &str) -> String {
    let title_prefix = title.split('(').next().unwrap_or(title);
    let normalized_title = title_prefix
        .chars()
        .filter(|ch| !ch.is_ascii_punctuation())
        .collect::<String>();
    format!("{normalized_title}{episode}")
}

fn detect_log_matched_entry(message: &str, after_ordered: &[HistEntry]) -> Option<HistEntry> {
    let target = message.trim();
    for entry in after_ordered.iter().rev() {
        if ani_cli_log_key(&entry.title, &entry.ep) == target {
            return Some(entry.clone());
        }
    }
    None
}

fn detect_latest_watch_event_from_logs(
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

fn parse_episode_f64(ep: &str) -> Option<f64> {
    ep.trim().parse::<f64>().ok()
}

fn episode_labels_match(a: &str, b: &str) -> bool {
    let left = a.trim();
    let right = b.trim();
    if left == right {
        return true;
    }

    match (parse_episode_f64(left), parse_episode_f64(right)) {
        (Some(x), Some(y)) => (x - y).abs() < 0.000_001,
        _ => false,
    }
}

fn compare_episode_labels(a: &str, b: &str) -> Ordering {
    match (parse_episode_f64(a), parse_episode_f64(b)) {
        (Some(left), Some(right)) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

fn parse_mode_episode_labels(raw: &str, mode: &str) -> Option<Vec<String>> {
    let marker = format!("\"{mode}\":[");
    let start = raw.find(&marker)? + marker.len();
    let after = &raw[start..];
    let end = after.find(']')?;
    let chunk = &after[..end];

    let mut episodes = Vec::new();
    for token in chunk.split(',') {
        let trimmed = token.trim().trim_matches('"');
        if !trimmed.is_empty() && trimmed != "null" {
            episodes.push(trimmed.to_string());
        }
    }
    if episodes.is_empty() {
        None
    } else {
        Some(episodes)
    }
}

fn choose_episode_labels_candidate(
    candidates: Vec<Vec<String>>,
    total_hint: Option<u32>,
) -> Option<Vec<String>> {
    if candidates.is_empty() {
        return None;
    }
    if let Some(total) = total_hint {
        for candidate in &candidates {
            if candidate.len() as u32 == total {
                return Some(candidate.clone());
            }
        }
    }
    candidates.into_iter().max_by_key(|episodes| episodes.len())
}

fn fetch_episode_labels(ani_id: &str, total_hint: Option<u32>) -> Option<Vec<String>> {
    let query = "query ($showId: String!) { show( _id: $showId ) { _id availableEpisodesDetail }}";
    let variables = format!("{{\"showId\":\"{ani_id}\"}}");
    let output = ProcessCommand::new("curl")
        .arg("-e")
        .arg("https://allanime.to")
        .arg("-s")
        .arg("-G")
        .arg("https://api.allanime.day/api")
        .arg("--data-urlencode")
        .arg(format!("variables={variables}"))
        .arg("--data-urlencode")
        .arg(format!("query={query}"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let raw = String::from_utf8(output.stdout).ok()?;
    let mut candidates = Vec::new();
    if let Some(sub) = parse_mode_episode_labels(&raw, "sub") {
        candidates.push(sub);
    }
    if let Some(dub) = parse_mode_episode_labels(&raw, "dub") {
        candidates.push(dub);
    }
    let mut episodes = choose_episode_labels_candidate(candidates, total_hint)?;
    episodes.sort_by(|left, right| compare_episode_labels(left, right));
    Some(episodes)
}

fn replay_seed_episode(last_episode: &str, episode_list: Option<&[String]>) -> Option<String> {
    if let Some(episodes) = episode_list
        && let Some(idx) = episodes
            .iter()
            .position(|episode| episode_labels_match(episode, last_episode))
    {
        if idx > 0 {
            return episodes.get(idx - 1).cloned();
        }
        return None;
    }

    let current = parse_episode_u32(last_episode)?;
    if current > 1 {
        Some((current - 1).to_string())
    } else {
        None
    }
}

fn resolve_replay_seed_episode(item: &crate::db::SeenEntry) -> Option<String> {
    let total_hint = parse_title_and_total_eps(&item.title).1;
    let episodes = fetch_episode_labels(&item.ani_id, total_hint);
    replay_seed_episode(&item.last_episode, episodes.as_deref())
}

fn has_next_episode(
    last_episode: &str,
    total_episodes: Option<u32>,
    episode_list: Option<&[String]>,
) -> bool {
    if let Some(episodes) = episode_list
        && let Some(idx) = episodes
            .iter()
            .position(|episode| episode_labels_match(episode, last_episode))
    {
        return idx + 1 < episodes.len();
    }

    if let (Some(total), Some(current)) = (total_episodes, parse_episode_u32(last_episode)) {
        return current < total;
    }

    true
}

fn episode_ordinal_from_list(last_episode: &str, episodes: &[String]) -> Option<u32> {
    episodes
        .iter()
        .position(|episode| episode_labels_match(episode, last_episode))
        .map(|idx| (idx + 1) as u32)
}

fn episode_progress_position(
    last_episode: &str,
    total_episodes: u32,
    episode_list: Option<&[String]>,
) -> Option<u32> {
    if total_episodes == 0 {
        return None;
    }

    if let Some(episodes) = episode_list
        && let Some(ordinal) = episode_ordinal_from_list(last_episode, episodes)
    {
        return Some(ordinal.min(total_episodes));
    }

    parse_episode_u32(last_episode).map(|current| current.min(total_episodes))
}

fn format_episode_progress_text(
    last_episode: &str,
    total_episodes: u32,
    episode_list: Option<&[String]>,
) -> String {
    match episode_progress_position(last_episode, total_episodes, episode_list) {
        Some(position) => {
            if parse_episode_u32(last_episode) == Some(position) {
                format!("{position} of {total_episodes}")
            } else {
                format!("{position} of {total_episodes} (episode {last_episode})")
            }
        }
        None => format!("{last_episode} of {total_episodes}"),
    }
}

fn build_progress_gauge(
    last_episode: &str,
    total_episodes: u32,
    episode_list: Option<&[String]>,
) -> Option<(f64, String)> {
    let shown = episode_progress_position(last_episode, total_episodes, episode_list)?;
    let ratio = (shown as f64 / total_episodes as f64).clamp(0.0, 1.0);
    Some((ratio, format!("{shown}/{total_episodes}")))
}

fn truncate(s: &str, max: usize) -> String {
    let mut out = s.to_string();
    if out.chars().count() > max {
        out = out.chars().take(max.saturating_sub(3)).collect::<String>() + "...";
    }
    out
}

fn sanitize_title_for_search(title: &str) -> String {
    let trimmed = title.trim();
    if let Some(open_idx) = trimmed.rfind('(')
        && trimmed.ends_with(')')
        && trimmed[open_idx..].contains("episodes")
    {
        return trimmed[..open_idx].trim().to_string();
    }
    trimmed.to_string()
}

fn parse_episode_u32(ep: &str) -> Option<u32> {
    ep.trim().parse::<u32>().ok()
}

fn format_last_seen_display(raw: &str) -> String {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hist_line_accepts_valid_format() {
        let entry = parse_hist_line("12\tshow-123\tShow Title").expect("line should parse");
        assert_eq!(entry.ep, "12");
        assert_eq!(entry.id, "show-123");
        assert_eq!(entry.title, "Show Title");
    }

    #[test]
    fn parse_hist_line_accepts_space_separated_format_with_episode_zero() {
        let entry = parse_hist_line("0 show-0 Episode Zero Title").expect("line should parse");
        assert_eq!(entry.ep, "0");
        assert_eq!(entry.id, "show-0");
        assert_eq!(entry.title, "Episode Zero Title");
    }

    #[test]
    fn parse_hist_line_preserves_decimal_episode_value() {
        let entry = parse_hist_line("13.5\tshow-135\tMid-season OVA").expect("line should parse");
        assert_eq!(entry.ep, "13.5");
        assert_eq!(entry.id, "show-135");
    }

    #[test]
    fn parse_hist_map_ignores_malformed_lines() {
        let raw = "1\tid-1\tShow One\nbadline\n\tid-2\tMissing episode\n2\tid-2\tShow Two\n";
        let (parsed, ordered, skipped) = parse_hist_map(raw);
        assert_eq!(parsed.len(), 2);
        assert_eq!(ordered.len(), 2);
        assert_eq!(skipped, 2);
        assert_eq!(
            parsed.get("id-2").map(|entry| entry.title.as_str()),
            Some("Show Two")
        );
    }

    #[test]
    fn detect_changed_latest_returns_most_recent_changed_entry() {
        let mut before = HashMap::new();
        before.insert(
            "id-1".to_string(),
            HistEntry {
                ep: "1".to_string(),
                id: "id-1".to_string(),
                title: "Show One".to_string(),
            },
        );

        let after_ordered = vec![
            HistEntry {
                ep: "1".to_string(),
                id: "id-1".to_string(),
                title: "Show One".to_string(),
            },
            HistEntry {
                ep: "0".to_string(),
                id: "id-2".to_string(),
                title: "Show Two".to_string(),
            },
            HistEntry {
                ep: "2".to_string(),
                id: "id-1".to_string(),
                title: "Show One".to_string(),
            },
        ];

        let changed = detect_changed_latest(&before, &after_ordered)
            .expect("entry should be detected as changed");
        assert_eq!(changed.id, "id-1");
        assert_eq!(changed.ep, "2");
    }

    #[test]
    fn detect_changed_latest_handles_episode_zero() {
        let before = HashMap::new();
        let after_ordered = vec![HistEntry {
            ep: "0".to_string(),
            id: "id-0".to_string(),
            title: "Episode Zero Show".to_string(),
        }];

        let changed = detect_changed_latest(&before, &after_ordered)
            .expect("episode 0 entry should be treated as a valid change");
        assert_eq!(changed.id, "id-0");
        assert_eq!(changed.ep, "0");
    }

    #[test]
    fn detect_latest_watch_event_accepts_appended_duplicate_episode_zero() {
        let before_entry = HistEntry {
            ep: "0".to_string(),
            id: "id-0".to_string(),
            title: "Episode Zero Show".to_string(),
        };

        let mut before_map = HashMap::new();
        before_map.insert(before_entry.id.clone(), before_entry.clone());

        let before_ordered = vec![before_entry.clone()];
        let after_ordered = vec![before_entry.clone(), before_entry.clone()];

        let changed = detect_latest_watch_event(&before_map, &before_ordered, &after_ordered)
            .expect("appended duplicate entry should count as a watch event");
        assert_eq!(changed.id, "id-0");
        assert_eq!(changed.ep, "0");
    }

    #[test]
    fn detect_latest_watch_event_prefers_new_added_entry_over_unchanged_trailing_line() {
        let before_a = HistEntry {
            ep: "2".to_string(),
            id: "id-a".to_string(),
            title: "Show A".to_string(),
        };
        let before_b = HistEntry {
            ep: "7".to_string(),
            id: "id-b".to_string(),
            title: "Show B".to_string(),
        };
        let mut before_map = HashMap::new();
        before_map.insert(before_a.id.clone(), before_a.clone());
        before_map.insert(before_b.id.clone(), before_b.clone());

        let before_ordered = vec![before_a.clone(), before_b.clone()];
        let after_ordered = vec![
            before_a.clone(),
            before_b.clone(),
            HistEntry {
                ep: "0".to_string(),
                id: "id-new".to_string(),
                title: "Brand New Show".to_string(),
            },
            before_b.clone(),
        ];

        let changed = detect_latest_watch_event(&before_map, &before_ordered, &after_ordered)
            .expect("new appended entry should be selected");
        assert_eq!(changed.id, "id-new");
        assert_eq!(changed.ep, "0");
    }

    #[test]
    fn detect_latest_watch_event_returns_none_when_content_is_unchanged() {
        let before_entry = HistEntry {
            ep: "1".to_string(),
            id: "id-existing".to_string(),
            title: "Existing Show".to_string(),
        };
        let mut before_map = HashMap::new();
        before_map.insert(before_entry.id.clone(), before_entry.clone());

        let before_ordered = vec![before_entry.clone()];
        let after_ordered = vec![before_entry];

        let changed = detect_latest_watch_event(&before_map, &before_ordered, &after_ordered);
        assert!(changed.is_none());
    }

    #[test]
    fn history_file_touched_detects_metadata_change() {
        let before = Some(HistFileSig {
            len: 100,
            modified_ns: 1000,
        });
        let after = Some(HistFileSig {
            len: 100,
            modified_ns: 1001,
        });
        assert!(history_file_touched(before, after));
    }

    #[test]
    fn history_file_touched_ignores_same_metadata() {
        let sig = Some(HistFileSig {
            len: 100,
            modified_ns: 1000,
        });
        assert!(!history_file_touched(sig, sig));
    }

    #[test]
    fn added_entries_detects_inserted_and_duplicate_new_occurrences() {
        let before = vec![
            HistEntry {
                ep: "1".to_string(),
                id: "a".to_string(),
                title: "A".to_string(),
            },
            HistEntry {
                ep: "2".to_string(),
                id: "b".to_string(),
                title: "B".to_string(),
            },
        ];
        let after = vec![
            before[0].clone(),
            HistEntry {
                ep: "0".to_string(),
                id: "c".to_string(),
                title: "C".to_string(),
            },
            before[1].clone(),
            HistEntry {
                ep: "2".to_string(),
                id: "b".to_string(),
                title: "B".to_string(),
            },
        ];

        let added = added_entries(&before, &after);
        assert_eq!(added.len(), 2);
        assert_eq!(added[0].id, "c");
        assert_eq!(added[1].id, "b");
    }

    #[test]
    fn parse_journal_ani_cli_line_extracts_timestamp_and_message() {
        let line = "1772039324.974245 fedora ani-cli[407433]: Shingeki no Kyojin 0";
        let (ts_ns, msg) = parse_journal_ani_cli_line(line).expect("line should parse");
        assert_eq!(msg, "Shingeki no Kyojin 0");
        assert_eq!(ts_ns, 1_772_039_324_974_245_000);
    }

    #[test]
    fn ani_cli_log_key_matches_ani_cli_logger_format() {
        let key = ani_cli_log_key("Death Note: Rewrite (1 episodes)", "1");
        assert_eq!(key, "Death Note Rewrite 1");
    }

    #[test]
    fn detect_log_matched_entry_handles_episode_zero() {
        let after_ordered = vec![
            HistEntry {
                ep: "1".to_string(),
                id: "id-1".to_string(),
                title: "Death Note (37 episodes)".to_string(),
            },
            HistEntry {
                ep: "0".to_string(),
                id: "id-2".to_string(),
                title: "Shingeki no Kyojin (27 episodes)".to_string(),
            },
        ];

        let matched = detect_log_matched_entry("Shingeki no Kyojin 0", &after_ordered)
            .expect("message should map to history entry");
        assert_eq!(matched.id, "id-2");
        assert_eq!(matched.ep, "0");
    }

    #[test]
    fn episode_ordinal_from_list_counts_zero_and_decimal_entries() {
        let mut episodes = vec!["0".to_string()];
        for ep in 1..=13 {
            episodes.push(ep.to_string());
        }
        episodes.push("13.5".to_string());
        for ep in 14..=25 {
            episodes.push(ep.to_string());
        }

        let ordinal =
            episode_ordinal_from_list("25", &episodes).expect("episode should be found in list");
        assert_eq!(ordinal, 27);
    }

    #[test]
    fn build_progress_gauge_uses_episode_ordinal_when_list_available() {
        let mut episodes = vec!["0".to_string()];
        for ep in 1..=13 {
            episodes.push(ep.to_string());
        }
        episodes.push("13.5".to_string());
        for ep in 14..=25 {
            episodes.push(ep.to_string());
        }

        let (ratio, label) = build_progress_gauge("25", 27, Some(&episodes))
            .expect("gauge should be generated");
        assert!((ratio - 1.0).abs() < 0.000_001);
        assert_eq!(label, "27/27");
    }

    #[test]
    fn build_progress_gauge_falls_back_to_numeric_episode_without_list() {
        let (ratio, label) =
            build_progress_gauge("25", 27, None).expect("numeric fallback should work");
        assert!((ratio - (25.0 / 27.0)).abs() < 0.000_001);
        assert_eq!(label, "25/27");
    }

    #[test]
    fn format_episode_progress_text_uses_ordinal_and_keeps_raw_label_when_needed() {
        let mut episodes = vec!["0".to_string()];
        for ep in 1..=13 {
            episodes.push(ep.to_string());
        }
        episodes.push("13.5".to_string());
        for ep in 14..=25 {
            episodes.push(ep.to_string());
        }

        let text = format_episode_progress_text("25", 27, Some(&episodes));
        assert_eq!(text, "27 of 27 (episode 25)");
    }

    #[test]
    fn format_episode_progress_text_uses_plain_numeric_when_ordinal_matches() {
        let text = format_episode_progress_text("12", 24, None);
        assert_eq!(text, "12 of 24");
    }

    #[test]
    fn replay_seed_episode_uses_previous_episode_from_list() {
        let episodes = vec![
            "0".to_string(),
            "1".to_string(),
            "2".to_string(),
            "13".to_string(),
            "13.5".to_string(),
        ];

        let seed = replay_seed_episode("13.5", Some(&episodes));
        assert_eq!(seed.as_deref(), Some("13"));
    }

    #[test]
    fn replay_seed_episode_none_for_first_episode_in_list() {
        let episodes = vec!["0".to_string(), "1".to_string(), "2".to_string()];
        let seed = replay_seed_episode("0", Some(&episodes));
        assert!(seed.is_none());
    }

    #[test]
    fn replay_seed_episode_falls_back_to_numeric_when_list_missing() {
        let seed = replay_seed_episode("5", None);
        assert_eq!(seed.as_deref(), Some("4"));

        let seed_first = replay_seed_episode("1", None);
        assert!(seed_first.is_none());
    }

    #[test]
    fn has_next_episode_uses_episode_list_for_non_linear_numbering() {
        let mut episodes = vec!["0".to_string()];
        for ep in 1..=13 {
            episodes.push(ep.to_string());
        }
        episodes.push("13.5".to_string());
        for ep in 14..=25 {
            episodes.push(ep.to_string());
        }

        assert!(!has_next_episode("25", Some(27), Some(&episodes)));
        assert!(has_next_episode("24", Some(27), Some(&episodes)));
    }

    #[test]
    fn has_next_episode_falls_back_to_numeric_when_list_missing() {
        assert!(has_next_episode("25", Some(27), None));
        assert!(!has_next_episode("27", Some(27), None));
    }

    #[test]
    fn format_last_seen_display_parses_rfc3339_timestamp() {
        let formatted = format_last_seen_display("2026-02-25T18:27:06.100701256+00:00");
        assert_eq!(formatted, "2026-02-25 18:27");
    }

    #[test]
    fn format_last_seen_display_keeps_raw_when_invalid() {
        let raw = "not-a-timestamp";
        assert_eq!(format_last_seen_display(raw), raw);
    }
}
