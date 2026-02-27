use std::cmp::Ordering;
use std::process::Command as ProcessCommand;

use chrono::{DateTime, Local};
use serde_json::Value;

pub(crate) fn parse_title_and_total_eps(title: &str) -> (String, Option<u32>) {
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

pub(crate) fn fetch_episode_labels(ani_id: &str, total_hint: Option<u32>) -> Option<Vec<String>> {
    let query = "query ($showId: String!) { show( _id: $showId ) { _id availableEpisodesDetail }}";
    let variables = format!("{{\"showId\":\"{ani_id}\"}}");
    let output = ProcessCommand::new("curl")
        .arg("-e")
        .arg("https://allanime.to")
        .arg("-sS")
        .arg("--retry")
        .arg("2")
        .arg("--retry-delay")
        .arg("1")
        .arg("--connect-timeout")
        .arg("3")
        .arg("--max-time")
        .arg("5")
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
    let parsed: Value = serde_json::from_str(&raw).ok()?;
    let mut candidates = Vec::new();
    if let Some(sub) = parse_mode_episode_labels_from_value(&parsed, "sub") {
        candidates.push(sub);
    }
    if let Some(dub) = parse_mode_episode_labels_from_value(&parsed, "dub") {
        candidates.push(dub);
    }
    let mut episodes = choose_episode_labels_candidate(candidates, total_hint)?;
    episodes.sort_by(|left, right| compare_episode_labels(left, right));
    Some(episodes)
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
    if let Some(open_idx) = trimmed.rfind('(')
        && trimmed.ends_with(')')
        && trimmed[open_idx..].contains("episodes")
    {
        return trimmed[..open_idx].trim().to_string();
    }
    trimmed.to_string()
}

pub(crate) fn parse_episode_u32(ep: &str) -> Option<u32> {
    ep.trim().parse::<u32>().ok()
}

pub(crate) fn format_last_seen_display(raw: &str) -> String {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| {
            dt.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M %:z")
                .to_string()
        })
        .unwrap_or_else(|_| raw.to_string())
}
