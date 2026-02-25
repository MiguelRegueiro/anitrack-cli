use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "anitrack",
    version,
    about = "Launch ani-cli and track last seen show/episode"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Start,
    Next,
    Replay,
    List,
    Tui,
}
