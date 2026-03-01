mod episode;
mod tracking;
mod tui;

#[cfg(test)]
mod tests;

use anyhow::Result;

use crate::cli::{Cli, Command};
use crate::db::Database;
use crate::paths::database_file_path;

use self::episode::{format_last_seen_display, truncate};
use self::tracking::{run_ani_cli_continue, run_ani_cli_replay, run_ani_cli_search};

pub fn run(cli: Cli) -> Result<()> {
    let db = open_db()?;

    match cli.command {
        Some(Command::Start) => run_start(&db)?,
        Some(Command::Next) => run_next(&db)?,
        Some(Command::Replay) => run_replay(&db)?,
        Some(Command::List) => run_list(&db)?,
        Some(Command::Tui) | None => tui::run_tui(&db)?,
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
                println!("{}", playback_failure_message(&outcome));
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

            let outcome = run_ani_cli_replay(&item, None);
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
                println!("{}", playback_failure_message(&outcome));
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

fn open_db() -> Result<Database> {
    let db_path = database_file_path()?;
    let db = Database::open(&db_path)?;
    db.migrate()?;
    Ok(db)
}

fn playback_failure_message(outcome: &tracking::PlaybackOutcome) -> String {
    match outcome.failure_detail.as_deref() {
        Some(detail) => format!("Playback failed/interrupted: {detail}. Progress not updated."),
        None => "Playback failed/interrupted. Progress not updated.".to_string(),
    }
}
