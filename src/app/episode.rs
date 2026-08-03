use std::cmp::Ordering;
use std::time::Duration;

use chrono::{DateTime, Local};
use serde_json::{Value, json};

use crate::http::post_json_with_retries;

pub(crate) fn parse_title_and_total_eps(title: &str) -> (String, Option<u32>) {
    let trimmed = title.trim();
    let mut remaining = trimmed;
    let mut total = None;

    // Older ani-cli history titles can end in both an episode count and a year,
    // for example "Show (12 episodes) (2024)". Walk all recognized metadata
    // suffixes so those entries remain searchable after ani-cli changes source.
    while let Some((prefix, suffix)) = split_trailing_parenthetical(remaining) {
        if let Some(value) = suffix
            .strip_suffix(" episodes")
            .and_then(|raw| raw.trim().parse::<u32>().ok())
        {
            total = Some(value);
            remaining = prefix;
            continue;
        }
        if is_year_suffix(suffix) {
            remaining = prefix;
            continue;
        }
        break;
    }

    if total.is_some() {
        (remaining.to_string(), total)
    } else {
        (trimmed.to_string(), None)
    }
}

fn split_trailing_parenthetical(raw: &str) -> Option<(&str, &str)> {
    let trimmed = raw.trim();
    if !trimmed.ends_with(')') {
        return None;
    }
    let open_idx = trimmed.rfind('(')?;
    let prefix = trimmed[..open_idx].trim_end();
    let suffix = trimmed[open_idx + 1..trimmed.len() - 1].trim();
    Some((prefix, suffix))
}

fn is_year_suffix(raw: &str) -> bool {
    raw.len() == 4 && raw.bytes().all(|byte| byte.is_ascii_digit())
}

pub(crate) fn parse_episode_f64(ep: &str) -> Option<f64> {
    ep.trim().parse::<f64>().ok()
}

pub(crate) fn episode_labels_match(a: &str, b: &str) -> bool {
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

pub(crate) fn compare_episode_labels(a: &str, b: &str) -> Ordering {
    match (parse_episode_f64(a), parse_episode_f64(b)) {
        (Some(left), Some(right)) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

#[cfg(test)]
pub(crate) fn parse_mode_episode_labels(raw: &str, mode: &str) -> Option<Vec<String>> {
    let value: Value = serde_json::from_str(raw).ok()?;
    parse_mode_episode_labels_from_value(&value, mode)
}

fn parse_mode_episode_labels_from_value(value: &Value, mode: &str) -> Option<Vec<String>> {
    let items = value
        .pointer("/data/show/availableEpisodesDetail")?
        .get(mode)?
        .as_array()?;

    let mut episodes = Vec::new();
    for item in items {
        if item.is_null() {
            continue;
        }

        let value = match item {
            Value::String(text) => text.trim().to_string(),
            Value::Number(number) => number.to_string(),
            _ => continue,
        };

        if !value.is_empty() && value != "null" {
            episodes.push(value);
        }
    }
    if episodes.is_empty() {
        None
    } else {
        Some(episodes)
    }
}

pub(crate) fn choose_episode_labels_candidate(
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

pub(crate) fn fallback_numeric_episode_labels(total_hint: Option<u32>) -> Option<Vec<String>> {
    let total = total_hint?;
    (total > 0).then(|| (1..=total).map(|episode| episode.to_string()).collect())
}

#[derive(Debug, Clone, Default)]
pub(crate) struct EpisodeLabelFetchOutcome {
    pub(crate) episode_list: Option<Vec<String>>,
    pub(crate) warnings: Vec<String>,
}

pub(crate) fn fetch_episode_labels_with_diagnostics(
    ani_id: &str,
    total_hint: Option<u32>,
) -> EpisodeLabelFetchOutcome {
    let query = "query ($showId: String!) { show( _id: $showId ) { _id availableEpisodesDetail }}";
    let body = json!({
        "variables": {
            "showId": ani_id,
        },
        "query": query,
    })
    .to_string();
    let raw = match post_json_with_retries(
        "https://api.allanime.day/api",
        "https://allmanga.to",
        &body,
        Duration::from_secs(3),
        Duration::from_secs(5),
        3,
        Duration::from_secs(1),
    ) {
        Ok(raw) => raw,
        Err(err) => {
            if let Some(episode_list) = fallback_numeric_episode_labels(total_hint) {
                return EpisodeLabelFetchOutcome {
                    episode_list: Some(episode_list),
                    warnings: Vec::new(),
                };
            }
            let warning = format!("episode metadata request failed for {ani_id}: {err}");
            return EpisodeLabelFetchOutcome {
                episode_list: None,
                warnings: vec![warning],
            };
        }
    };

    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(err) => {
            return EpisodeLabelFetchOutcome {
                episode_list: None,
                warnings: vec![format!(
                    "episode metadata response parse failed for {ani_id}: {err}"
                )],
            };
        }
    };

    let mut candidates = Vec::new();
    if let Some(sub) = parse_mode_episode_labels_from_value(&parsed, "sub") {
        candidates.push(sub);
    }
    if let Some(dub) = parse_mode_episode_labels_from_value(&parsed, "dub") {
        candidates.push(dub);
    }
    let Some(mut episodes) = choose_episode_labels_candidate(candidates, total_hint) else {
        if let Some(episode_list) = fallback_numeric_episode_labels(total_hint) {
            return EpisodeLabelFetchOutcome {
                episode_list: Some(episode_list),
                warnings: Vec::new(),
            };
        }
        return EpisodeLabelFetchOutcome {
            episode_list: None,
            warnings: vec![format!(
                "episode metadata response for {ani_id} did not contain usable sub/dub episode labels"
            )],
        };
    };
    episodes.sort_by(|left, right| compare_episode_labels(left, right));
    EpisodeLabelFetchOutcome {
        episode_list: Some(episodes),
        warnings: Vec::new(),
    }
}

pub(crate) fn replay_seed_episode(
    last_episode: &str,
    episode_list: Option<&[String]>,
) -> Option<String> {
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

pub(crate) fn next_target_episode(
    last_episode: &str,
    episode_list: Option<&[String]>,
) -> Option<String> {
    if let Some(episodes) = episode_list
        && let Some(idx) = episodes
            .iter()
            .position(|episode| episode_labels_match(episode, last_episode))
    {
        return episodes.get(idx + 1).cloned();
    }

    let current = parse_episode_f64(last_episode)?;
    if current < 0.0 {
        return None;
    }
    integer_episode_label(current.floor() + 1.0)
}

pub(crate) fn previous_target_episode(
    last_episode: &str,
    episode_list: Option<&[String]>,
) -> Option<String> {
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

    let current = parse_episode_f64(last_episode)?;
    if current <= 0.0 {
        return None;
    }

    if is_effective_integer(current) {
        return integer_episode_label(current - 1.0);
    }

    integer_episode_label(current.floor())
}

pub(crate) fn previous_seed_episode(
    last_episode: &str,
    episode_list: Option<&[String]>,
) -> Option<String> {
    if let Some(episodes) = episode_list
        && let Some(idx) = episodes
            .iter()
            .position(|episode| episode_labels_match(episode, last_episode))
    {
        if idx > 1 {
            return episodes.get(idx - 2).cloned();
        }
        return None;
    }

    let target = previous_target_episode(last_episode, None)?;
    let target_value = parse_episode_f64(&target)?;
    if target_value > 1.0 {
        integer_episode_label(target_value - 1.0)
    } else {
        None
    }
}

pub(crate) fn has_next_episode(
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

pub(crate) fn display_total_episodes(
    last_episode: &str,
    stored_total: Option<u32>,
    title_total: Option<u32>,
    episode_list: Option<&[String]>,
) -> Option<u32> {
    let mut total = episode_list
        .and_then(|episodes| u32::try_from(episodes.len()).ok())
        .filter(|count| *count > 0)
        .or(stored_total)
        .or(title_total);

    if let Some(last) = parse_episode_u32(last_episode) {
        total = Some(total.map_or(last, |current| current.max(last)));
    }

    total
}

pub(crate) fn has_previous_episode(last_episode: &str, episode_list: Option<&[String]>) -> bool {
    previous_target_episode(last_episode, episode_list).is_some()
}

pub(crate) fn integer_episode_label(value: f64) -> Option<String> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let rounded = value.round();
    if !is_effective_integer(rounded) {
        return None;
    }
    Some(format!("{}", rounded as i64))
}

pub(crate) fn is_effective_integer(value: f64) -> bool {
    (value - value.round()).abs() < 0.000_001
}

pub(crate) fn episode_ordinal_from_list(last_episode: &str, episodes: &[String]) -> Option<u32> {
    episodes
        .iter()
        .position(|episode| episode_labels_match(episode, last_episode))
        .map(|idx| (idx + 1) as u32)
}

pub(crate) fn episode_progress_position(
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

pub(crate) fn format_episode_progress_text(
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

pub(crate) fn build_progress_gauge(
    last_episode: &str,
    total_episodes: u32,
    episode_list: Option<&[String]>,
) -> Option<(f64, String)> {
    let shown = episode_progress_position(last_episode, total_episodes, episode_list)?;
    let ratio = (shown as f64 / total_episodes as f64).clamp(0.0, 1.0);
    Some((ratio, format!("{shown}/{total_episodes}")))
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    let mut out = s.to_string();
    if out.chars().count() > max {
        out = out.chars().take(max.saturating_sub(3)).collect::<String>() + "...";
    }
    out
}

pub(crate) fn sanitize_title_for_search(title: &str) -> String {
    let trimmed = title.trim();
    let mut remaining = trimmed;
    let mut removed_metadata = false;

    while let Some((prefix, suffix)) = split_trailing_parenthetical(remaining) {
        let is_episode_count = suffix
            .strip_suffix(" episodes")
            .is_some_and(|raw| raw.trim().parse::<u32>().is_ok());
        if !is_episode_count && !is_year_suffix(suffix) {
            break;
        }
        remaining = prefix;
        removed_metadata = true;
    }

    if removed_metadata {
        remaining.to_string()
    } else {
        trimmed.to_string()
    }
}

pub(crate) fn parse_episode_u32(ep: &str) -> Option<u32> {
    ep.trim().parse::<u32>().ok()
}

pub(crate) fn format_last_seen_display(raw: &str) -> String {
    format_last_seen_display_with_pattern(raw, "%Y-%m-%d %H:%M %:z")
}

pub(crate) fn format_last_seen_display_tui(raw: &str) -> String {
    format_last_seen_display_with_pattern(raw, "%Y-%m-%d %H:%M")
}

fn format_last_seen_display_with_pattern(raw: &str, pattern: &str) -> String {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Local).format(pattern).to_string())
        .unwrap_or_else(|_| raw.to_string())
}
