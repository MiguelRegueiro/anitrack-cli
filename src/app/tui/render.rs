use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Gauge, Padding, Paragraph, Row, Table, TableState,
    Wrap,
};

use crate::db::SeenEntry;

use super::super::episode::{
    build_progress_gauge, format_episode_progress_text, format_last_seen_display_tui,
    parse_title_and_total_eps, truncate,
};
use super::{EpisodeListState, PendingDelete, PendingNotice, TuiAction};

struct LibraryColumns {
    headers: Vec<&'static str>,
    widths: Vec<Constraint>,
    show_last_seen: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_tui(
    frame: &mut Frame,
    items: &[SeenEntry],
    table_state: &mut TableState,
    action: TuiAction,
    dub: bool,
    status: &str,
    pending_delete: Option<&PendingDelete>,
    pending_notice: Option<&PendingNotice>,
    episode_lists_by_id: &HashMap<String, EpisodeListState>,
) {
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
        if dub {
            Span::styled("   DUB", Style::default().fg(Color::Magenta))
        } else {
            Span::raw("")
        },
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

    let library_columns = library_columns(body_chunks[0].width);
    let rows: Vec<Row> = items
        .iter()
        .map(|item| {
            let (display_title, total_eps) = parse_title_and_total_eps(&item.title);
            let total_eps = total_eps
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".to_string());
            let mut cells = vec![
                Cell::from(display_title),
                Cell::from(total_eps),
                Cell::from(item.last_episode.clone()),
            ];
            if library_columns.show_last_seen {
                cells.push(Cell::from(format_last_seen_display_tui(&item.last_seen_at)));
            }
            Row::new(cells)
        })
        .collect();

    let table = Table::new(rows, library_columns.widths)
        .header(
            Row::new(library_columns.headers).style(
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
                format_last_seen_display_tui(&item.last_seen_at),
            );
            if episode_state.is_some_and(EpisodeListState::is_loading) {
                selection_text.push_str("\n\nEpisodes\nLoading...");
            }
            if let Some(warning) = episode_state.and_then(EpisodeListState::warning) {
                selection_text.push_str("\n\nMetadata warning\n");
                selection_text.push_str(&truncate(warning, 90));
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
        let progress_area = details_chunks[1];
        let progress = Gauge::default()
            .block(panel_block("Progress"))
            .gauge_style(
                Style::default()
                    .fg(Color::Rgb(130, 190, 255))
                    .add_modifier(Modifier::BOLD),
            )
            .label("")
            .ratio(ratio);
        frame.render_widget(progress, progress_area);
        render_progress_label(frame, progress_area, ratio, &label);
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
        frame.render_widget(Clear, popup_area);
        let popup = Paragraph::new(popup_text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(modal_block("Confirm Delete"));
        frame.render_widget(popup, popup_area);
    } else if let Some(notice) = pending_notice {
        let popup_area = popup_rect_for_text(frame.area(), &notice.message);
        frame.render_widget(Clear, popup_area);
        let popup = Paragraph::new(notice.message.clone())
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(modal_block("No More Episodes"));
        frame.render_widget(popup, popup_area);
    }
}

fn library_columns(width: u16) -> LibraryColumns {
    let content_width = width.saturating_sub(2).max(1);
    let eps_width = 5;
    let last_ep_width = 5;
    let fixed_width = eps_width + last_ep_width;

    if content_width >= 52 {
        let min_title_width = 28;
        let min_seen_width = 8;
        let preferred_seen_width = if content_width >= 82 { 20 } else { 12 };
        let available_seen_width = content_width.saturating_sub(fixed_width + min_title_width);
        let seen_width = available_seen_width.clamp(min_seen_width, preferred_seen_width);
        let title_width = content_width.saturating_sub(fixed_width + seen_width);

        return LibraryColumns {
            headers: if content_width >= 82 {
                vec!["Title", "Eps", "Last", "Last Seen"]
            } else {
                vec!["Title", "Eps", "Last", "Seen"]
            },
            widths: vec![
                Constraint::Length(title_width),
                Constraint::Length(eps_width),
                Constraint::Length(last_ep_width),
                Constraint::Length(seen_width),
            ],
            show_last_seen: true,
        };
    }

    let title_width = content_width.saturating_sub(fixed_width).max(1);
    LibraryColumns {
        headers: vec!["Title", "Eps", "Last"],
        widths: vec![
            Constraint::Length(title_width),
            Constraint::Length(eps_width),
            Constraint::Length(last_ep_width),
        ],
        show_last_seen: false,
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

fn render_progress_label(frame: &mut Frame, area: Rect, ratio: f64, label: &str) {
    let inner = Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    if inner.width == 0 || inner.height == 0 || label.is_empty() {
        return;
    }

    let label_width = (label.chars().count() as u16).min(inner.width);
    let label_x = inner.x + inner.width.saturating_sub(label_width) / 2;
    let label_y = inner.y + inner.height / 2;
    let filled_end = inner.x + (f64::from(inner.width) * ratio).round() as u16;

    for (offset, ch) in label.chars().take(label_width as usize).enumerate() {
        let x = label_x + offset as u16;
        let style = if x < filled_end {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(130, 190, 255))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Rgb(130, 190, 255))
                .add_modifier(Modifier::BOLD)
        };
        if let Some(cell) = frame.buffer_mut().cell_mut((x, label_y)) {
            cell.set_symbol(&ch.to_string()).set_style(style);
        }
    }
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
