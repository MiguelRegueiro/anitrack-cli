use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitStatus, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::{Frame, Terminal};

use crate::cli::{Cli, Command};
use crate::db::Database;
use crate::paths::database_file_path;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[derive(Debug, Clone)]
struct HistEntry {
    ep: String,
    id: String,
    title: String,
}

pub fn run(cli: Cli) -> Result<()> {
    let db = open_db()?;

    match cli.command {
        Command::Start => run_start(&db)?,
        Command::Next => run_next(&db)?,
        Command::Replay => run_replay(&db)?,
        Command::List => run_list(&db)?,
        Command::Tui => run_tui(&db)?,
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
            let current = item.last_episode.parse::<u32>().with_context(|| {
                format!(
                    "cannot parse last episode '{}' for '{}'",
                    item.last_episode, item.title
                )
            })?;
            let next = current + 1;
            println!("Playing next episode for last seen show:");
            println!("  Title: {}", item.title);
            println!("  Episode: {}", next);

            let outcome = run_ani_cli_continue(&item, current)?;
            if outcome.success {
                let updated_ep = outcome.final_episode.unwrap_or(next);
                db.upsert_seen(&item.ani_id, &item.title, &updated_ep.to_string())?;
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
            let current = item.last_episode.parse::<u32>().with_context(|| {
                format!(
                    "cannot parse last episode '{}' for '{}'",
                    item.last_episode, item.title
                )
            })?;
            println!("Replaying last seen episode:");
            println!("  Title: {}", item.title);
            println!("  Episode: {}", current);

            let outcome = if current > 1 {
                // ani-cli -c plays the episode after what's in history.
                run_ani_cli_continue(&item, current - 1)?
            } else {
                // Episode 1 cannot be represented as "previous" in history; run explicit query.
                run_ani_cli_episode_with_global_tracking(&item, 1)?
            };
            if outcome.success {
                let updated_ep = outcome.final_episode.unwrap_or(current);
                db.upsert_seen(&item.ani_id, &item.title, &updated_ep.to_string())?;
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
            item.last_seen_at
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum TuiAction {
    Next,
    Replay,
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
    let mut status = if items.is_empty() {
        "No tracked entries yet. Run `anitrack start` first.".to_string()
    } else {
        "Use Up/Down to select show. Left=Next, Right=Replay. Enter runs action. s=Search. q quits."
            .to_string()
    };

    loop {
        terminal.draw(|frame| draw_tui(frame, &items, &mut table_state, action, &status))?;

        if !event::poll(Duration::from_millis(200))? {
            continue;
        }

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
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
                        status = msg;
                        items = db.list_seen()?;
                        if items.is_empty() {
                            table_state.select(None);
                        } else if let Some(id) = changed_id {
                            let idx = items.iter().position(|item| item.ani_id == id).unwrap_or(0);
                            table_state.select(Some(idx));
                        } else {
                            table_state.select(table_state.selected().or(Some(0)));
                        }
                    }
                    Err(err) => status = format!("Search failed: {err}"),
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
            KeyCode::Enter => {
                let Some(selected) = table_state.selected() else {
                    continue;
                };
                if selected >= items.len() {
                    continue;
                }

                let selected_id = items[selected].ani_id.clone();
                let selected_title = items[selected].title.clone();

                session.suspend()?;
                let result = run_selected_action(db, &items[selected], action);
                session.resume()?;
                terminal.clear()?;

                match result {
                    Ok(msg) => status = msg,
                    Err(err) => status = format!("Action failed for {selected_title}: {err}"),
                }

                items = db.list_seen()?;
                if items.is_empty() {
                    table_state.select(None);
                } else {
                    let idx = items
                        .iter()
                        .position(|item| item.ani_id == selected_id)
                        .unwrap_or(0);
                    table_state.select(Some(idx));
                }
            }
            _ => {}
        }
    }

    terminal.show_cursor()?;
    session.leave()?;
    Ok(())
}

fn draw_tui(
    frame: &mut Frame,
    items: &[crate::db::SeenEntry],
    table_state: &mut TableState,
    action: TuiAction,
    status: &str,
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

    let header =
        Paragraph::new("AniTrack TUI").block(Block::default().borders(Borders::ALL).title("Title"));
    frame.render_widget(header, chunks[0]);

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
                Cell::from(item.last_seen_at.clone()),
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
        Row::new(vec!["Title", "Total Eps", "Last Ep", "Last Seen"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Tracked Shows (latest first)"),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .highlight_symbol(">> ");
    frame.render_stateful_widget(table, chunks[1], table_state);

    let action_text = match action {
        TuiAction::Next => "[Next]  Replay",
        TuiAction::Replay => "Next  [Replay]",
    };
    let action_widget = Paragraph::new(action_text).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Action (Left/Right, Enter to run)"),
    );
    frame.render_widget(action_widget, chunks[2]);

    let status_widget = Paragraph::new(status.to_string())
        .block(Block::default().borders(Borders::ALL).title("Status"));
    frame.render_widget(status_widget, chunks[3]);
}

fn run_selected_action(
    db: &Database,
    item: &crate::db::SeenEntry,
    action: TuiAction,
) -> Result<String> {
    match action {
        TuiAction::Next => {
            let current = item.last_episode.parse::<u32>().with_context(|| {
                format!(
                    "cannot parse last episode '{}' for '{}'",
                    item.last_episode, item.title
                )
            })?;
            let next = current + 1;
            let outcome = run_ani_cli_continue(item, current)?;
            if outcome.success {
                let updated_ep = outcome.final_episode.unwrap_or(next);
                db.upsert_seen(&item.ani_id, &item.title, &updated_ep.to_string())?;
                Ok(format!(
                    "Updated progress: {} -> episode {}",
                    item.title, updated_ep
                ))
            } else {
                Ok("Playback failed/interrupted. Progress not updated.".to_string())
            }
        }
        TuiAction::Replay => {
            let current = item.last_episode.parse::<u32>().with_context(|| {
                format!(
                    "cannot parse last episode '{}' for '{}'",
                    item.last_episode, item.title
                )
            })?;

            let outcome = if current > 1 {
                run_ani_cli_continue(item, current - 1)?
            } else {
                run_ani_cli_episode_with_global_tracking(item, 1)?
            };
            if outcome.success {
                let updated_ep = outcome.final_episode.unwrap_or(current);
                db.upsert_seen(&item.ani_id, &item.title, &updated_ep.to_string())?;
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
    let before = read_hist_map(&histfile)?;

    let ani_cli_bin = resolve_ani_cli_bin();
    let status = with_sigint_ignored(|| {
        let mut cmd = ProcessCommand::new(&ani_cli_bin);
        cmd.stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        run_interactive_cmd(cmd)
            .with_context(|| format!("failed to launch {}", ani_cli_bin.display()))
    })?;

    let after = read_hist_map(&histfile)?;
    let mut changed_id = None;
    let mut message = if let Some(changed) = detect_changed(&before, &after) {
        db.upsert_seen(&changed.id, &changed.title, &changed.ep)?;
        changed_id = Some(changed.id);
        format!(
            "Recorded last seen: {} | episode {}",
            changed.title, changed.ep
        )
    } else {
        "No new history entry detected from this run.".to_string()
    };

    if !status.success() {
        message = format!("{message}\nani-cli exited with status: {status}");
    }

    Ok((message, changed_id))
}

fn resolve_ani_cli_bin() -> PathBuf {
    PathBuf::from("ani-cli")
}

#[derive(Debug, Clone, Copy)]
struct PlaybackOutcome {
    success: bool,
    final_episode: Option<u32>,
}

fn run_ani_cli_continue(
    item: &crate::db::SeenEntry,
    stored_episode: u32,
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
        read_hist_map(&histfile)?
            .get(&item.ani_id)
            .and_then(|entry| parse_episode_u32(&entry.ep))
    } else {
        None
    };
    let _ = fs::remove_dir_all(&temp_hist_dir);
    Ok(PlaybackOutcome {
        success: status.success(),
        final_episode,
    })
}

fn run_ani_cli_episode(title: &str, episode: u32) -> Result<bool> {
    let ani_cli_bin = resolve_ani_cli_bin();
    let status = ProcessCommand::new(&ani_cli_bin)
        .arg(title)
        .arg("-e")
        .arg(episode.to_string())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to launch {}", ani_cli_bin.display()))?;
    Ok(status.success())
}

fn run_ani_cli_episode_with_global_tracking(
    item: &crate::db::SeenEntry,
    episode: u32,
) -> Result<PlaybackOutcome> {
    let histfile = ani_cli_histfile();
    let before = read_hist_map(&histfile)?;
    let success = run_ani_cli_episode(&sanitize_title_for_search(&item.title), episode)?;
    let final_episode = if success {
        let after = read_hist_map(&histfile)?;
        after
            .get(&item.ani_id)
            .or_else(|| before.get(&item.ani_id))
            .and_then(|entry| parse_episode_u32(&entry.ep))
    } else {
        None
    };

    Ok(PlaybackOutcome {
        success,
        final_episode,
    })
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

fn read_hist_map(path: &Path) -> Result<HashMap<String, HistEntry>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read ani-cli history at {}", path.display()))?;

    let mut map = HashMap::new();
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, '\t');
        let ep = match parts.next() {
            Some(v) => v.trim(),
            None => continue,
        };
        let id = match parts.next() {
            Some(v) => v.trim(),
            None => continue,
        };
        let title = match parts.next() {
            Some(v) => v.trim(),
            None => continue,
        };
        if ep.is_empty() || id.is_empty() || title.is_empty() {
            continue;
        }
        map.insert(
            id.to_string(),
            HistEntry {
                ep: ep.to_string(),
                id: id.to_string(),
                title: title.to_string(),
            },
        );
    }
    Ok(map)
}

fn detect_changed(
    before: &HashMap<String, HistEntry>,
    after: &HashMap<String, HistEntry>,
) -> Option<HistEntry> {
    let mut changed = Vec::new();

    for (id, current) in after {
        match before.get(id) {
            None => changed.push(current.clone()),
            Some(prev) if prev.ep != current.ep || prev.title != current.title => {
                changed.push(current.clone())
            }
            _ => {}
        }
    }

    changed.into_iter().next()
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
