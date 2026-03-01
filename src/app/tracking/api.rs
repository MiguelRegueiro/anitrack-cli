use std::env;
use std::process::Command as ProcessCommand;

use serde_json::Value;

use super::super::episode::{parse_title_and_total_eps, sanitize_title_for_search};
use crate::db::SeenEntry;

pub(crate) fn resolve_select_nth_for_item(item: &SeenEntry) -> Option<u32> {
    #[cfg(test)]
    if let Some(override_index) = resolve_select_nth_test_override() {
        return Some(override_index);
    }

    let cleaned_title = sanitize_title_for_search(&item.title);
    let raw_title = item.title.trim().to_string();
    let queries = if cleaned_title == raw_title {
        vec![cleaned_title]
    } else {
        vec![cleaned_title, raw_title]
    };

    let env_mode = env::var("ANI_CLI_MODE").unwrap_or_else(|_| "sub".to_string());
    let mut modes = vec![env_mode, "sub".to_string(), "dub".to_string()];
    modes.dedup();

    for query in queries {
        for mode in &modes {
            let Some(entries) = fetch_search_result_entries(&query, mode) else {
                continue;
            };
            if let Some(index) = find_select_nth_index_by_id(&entries, &item.ani_id) {
                return Some(index);
            }
            if let Some(index) = find_select_nth_index_by_title(&entries, &item.title) {
                return Some(index);
            }
        }
    }
    None
}

#[cfg(test)]
fn resolve_select_nth_test_override() -> Option<u32> {
    let raw = env::var("ANI_TRACK_TEST_SELECT_NTH").ok()?;
    let parsed = raw.trim().parse::<u32>().ok()?;
    (parsed > 0).then_some(parsed)
}

pub(crate) fn fetch_search_result_entries(
    query: &str,
    mode: &str,
) -> Option<Vec<SearchResultEntry>> {
    let gql = "query( $search: SearchInput $limit: Int $page: Int $translationType: VaildTranslationTypeEnumType $countryOrigin: VaildCountryOriginEnumType ) { shows( search: $search limit: $limit page: $page translationType: $translationType countryOrigin: $countryOrigin ) { edges { _id name availableEpisodes __typename } }}";
    let escaped_query = json_escape(query);
    let escaped_mode = json_escape(mode);
    let variables = format!(
        "{{\"search\":{{\"allowAdult\":false,\"allowUnknown\":false,\"query\":\"{escaped_query}\"}},\"limit\":40,\"page\":1,\"translationType\":\"{escaped_mode}\",\"countryOrigin\":\"ALL\"}}"
    );
    let output = ProcessCommand::new("curl")
        .arg("-e")
        .arg("https://allmanga.to")
        .arg("-sS")
        .arg("--retry")
        .arg("2")
        .arg("--retry-delay")
        .arg("1")
        .arg("--connect-timeout")
        .arg("3")
        .arg("--max-time")
        .arg("6")
        .arg("-G")
        .arg("https://api.allanime.day/api")
        .arg("--data-urlencode")
        .arg(format!("variables={variables}"))
        .arg("--data-urlencode")
        .arg(format!("query={gql}"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let raw = String::from_utf8(output.stdout).ok()?;
    let entries = parse_search_result_entries(&raw);
    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SearchResultEntry {
    pub(crate) id: String,
    pub(crate) title: String,
}

pub(crate) fn parse_search_result_entries(raw: &str) -> Vec<SearchResultEntry> {
    let parsed: Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let Some(edges) = parsed
        .pointer("/data/shows/edges")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };

    edges
        .iter()
        .filter_map(|edge| {
            let id = edge.get("_id")?.as_str()?.trim();
            let title = edge.get("name")?.as_str()?.trim();
            if id.is_empty() || title.is_empty() {
                return None;
            }
            Some(SearchResultEntry {
                id: id.to_string(),
                title: title.to_string(),
            })
        })
        .collect()
}

pub(crate) fn find_select_nth_index_by_id(
    entries: &[SearchResultEntry],
    ani_id: &str,
) -> Option<u32> {
    entries
        .iter()
        .position(|entry| entry.id == ani_id)
        .map(|idx| (idx + 1) as u32)
}

pub(crate) fn find_select_nth_index_by_title(
    entries: &[SearchResultEntry],
    title: &str,
) -> Option<u32> {
    let target = normalize_title_for_match(title);
    entries
        .iter()
        .position(|entry| normalize_title_for_match(&entry.title) == target)
        .map(|idx| (idx + 1) as u32)
}

pub(crate) fn normalize_title_for_match(raw: &str) -> String {
    let base = parse_title_and_total_eps(raw).0;
    base.to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch.is_whitespace() {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn json_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let code = c as u32;
                out.push_str(&format!("\\u{code:04x}"));
            }
            c => out.push(c),
        }
    }
    out
}
