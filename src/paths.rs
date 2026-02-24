use std::path::PathBuf;

use anyhow::{Context, Result};

pub fn database_file_path() -> Result<PathBuf> {
    let base = dirs::data_dir().context("unable to resolve data directory")?;
    Ok(base.join("anitrack").join("anitrack.db"))
}
