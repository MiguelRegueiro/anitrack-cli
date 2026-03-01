use std::io;

use anyhow::{Context, Result};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

pub(super) struct TuiSession {
    active: bool,
}

impl TuiSession {
    pub(super) fn enter() -> Result<Self> {
        enable_raw_mode().context("failed to enable raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen).context("failed to enter alternate screen")?;
        Ok(Self { active: true })
    }

    pub(super) fn suspend(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        disable_raw_mode().context("failed to disable raw mode")?;
        execute!(io::stdout(), LeaveAlternateScreen).context("failed to leave alternate screen")?;
        self.active = false;
        Ok(())
    }

    pub(super) fn resume(&mut self) -> Result<()> {
        if self.active {
            return Ok(());
        }
        execute!(io::stdout(), EnterAlternateScreen)
            .context("failed to re-enter alternate screen")?;
        enable_raw_mode().context("failed to re-enable raw mode")?;
        self.active = true;
        Ok(())
    }

    pub(super) fn leave(&mut self) -> Result<()> {
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
