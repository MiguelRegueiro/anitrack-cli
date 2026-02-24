mod app;
mod cli;
mod db;
mod paths;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    app::run(cli)
}
