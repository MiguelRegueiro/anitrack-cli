mod api;
mod history;
mod playback;
mod process;

#[cfg(test)]
pub(crate) use api::*;
#[cfg(test)]
pub(crate) use history::*;
pub(crate) use playback::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct HistEntry {
    pub(crate) ep: String,
    pub(crate) id: String,
    pub(crate) title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HistFileSig {
    pub(crate) len: u64,
    pub(crate) modified_ns: u128,
}

#[derive(Debug, Clone)]
pub(crate) struct PlaybackOutcome {
    pub(crate) success: bool,
    pub(crate) final_episode: Option<String>,
    pub(crate) failure_detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReplayPlan {
    Continue {
        seed_episode: String,
    },
    Episode {
        episode: String,
        select_nth: Option<u32>,
    },
}
