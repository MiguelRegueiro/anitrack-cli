use std::collections::HashMap;
#[cfg(any(unix, windows))]
use std::ffi::OsStr;
use std::ffi::OsString;

#[cfg(any(unix, windows))]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(any(unix, windows))]
use std::path::{Path, PathBuf};
#[cfg(any(unix, windows))]
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Local};

#[cfg(any(unix, windows))]
use crate::db::Database;

use super::episode::*;
#[cfg(unix)]
use super::run_replay;
use super::tracking::*;
use super::tui::TuiAction;
#[cfg(any(unix, windows))]
use super::{run_next, run_start};

#[test]
fn parse_hist_line_accepts_valid_format() {
    let entry = parse_hist_line("12\tshow-123\tShow Title").expect("line should parse");
    assert_eq!(entry.ep, "12");
    assert_eq!(entry.id, "show-123");
    assert_eq!(entry.title, "Show Title");
}

#[test]
fn parse_hist_line_accepts_space_separated_format_with_episode_zero() {
    let entry = parse_hist_line("0 show-0 Episode Zero Title").expect("line should parse");
    assert_eq!(entry.ep, "0");
    assert_eq!(entry.id, "show-0");
    assert_eq!(entry.title, "Episode Zero Title");
}

#[test]
fn parse_hist_line_preserves_decimal_episode_value() {
    let entry = parse_hist_line("13.5\tshow-135\tMid-season OVA").expect("line should parse");
    assert_eq!(entry.ep, "13.5");
    assert_eq!(entry.id, "show-135");
}

#[test]
fn parse_hist_map_ignores_malformed_lines() {
    let raw = "1\tid-1\tShow One\nbadline\n\tid-2\tMissing episode\n2\tid-2\tShow Two\n";
    let (parsed, ordered, skipped) = parse_hist_map(raw);
    assert_eq!(parsed.len(), 2);
    assert_eq!(ordered.len(), 2);
    assert_eq!(skipped, 2);
    assert_eq!(
        parsed.get("id-2").map(|entry| entry.title.as_str()),
        Some("Show Two")
    );
}

#[test]
fn detect_changed_latest_returns_most_recent_changed_entry() {
    let mut before = HashMap::new();
    before.insert(
        "id-1".to_string(),
        HistEntry {
            ep: "1".to_string(),
            id: "id-1".to_string(),
            title: "Show One".to_string(),
        },
    );

    let after_ordered = vec![
        HistEntry {
            ep: "1".to_string(),
            id: "id-1".to_string(),
            title: "Show One".to_string(),
        },
        HistEntry {
            ep: "0".to_string(),
            id: "id-2".to_string(),
            title: "Show Two".to_string(),
        },
        HistEntry {
            ep: "2".to_string(),
            id: "id-1".to_string(),
            title: "Show One".to_string(),
        },
    ];

    let changed = detect_changed_latest(&before, &after_ordered)
        .expect("entry should be detected as changed");
    assert_eq!(changed.id, "id-1");
    assert_eq!(changed.ep, "2");
}

#[test]
fn detect_changed_latest_handles_episode_zero() {
    let before = HashMap::new();
    let after_ordered = vec![HistEntry {
        ep: "0".to_string(),
        id: "id-0".to_string(),
        title: "Episode Zero Show".to_string(),
    }];

    let changed = detect_changed_latest(&before, &after_ordered)
        .expect("episode 0 entry should be treated as a valid change");
    assert_eq!(changed.id, "id-0");
    assert_eq!(changed.ep, "0");
}

#[test]
fn detect_latest_watch_event_accepts_appended_duplicate_episode_zero() {
    let before_entry = HistEntry {
        ep: "0".to_string(),
        id: "id-0".to_string(),
        title: "Episode Zero Show".to_string(),
    };

    let mut before_map = HashMap::new();
    before_map.insert(before_entry.id.clone(), before_entry.clone());

    let before_ordered = vec![before_entry.clone()];
    let after_ordered = vec![before_entry.clone(), before_entry.clone()];

    let changed = detect_latest_watch_event(&before_map, &before_ordered, &after_ordered)
        .expect("appended duplicate entry should count as a watch event");
    assert_eq!(changed.id, "id-0");
    assert_eq!(changed.ep, "0");
}

#[test]
fn detect_latest_watch_event_prefers_new_added_entry_over_unchanged_trailing_line() {
    let before_a = HistEntry {
        ep: "2".to_string(),
        id: "id-a".to_string(),
        title: "Show A".to_string(),
    };
    let before_b = HistEntry {
        ep: "7".to_string(),
        id: "id-b".to_string(),
        title: "Show B".to_string(),
    };
    let mut before_map = HashMap::new();
    before_map.insert(before_a.id.clone(), before_a.clone());
    before_map.insert(before_b.id.clone(), before_b.clone());

    let before_ordered = vec![before_a.clone(), before_b.clone()];
    let after_ordered = vec![
        before_a.clone(),
        before_b.clone(),
        HistEntry {
            ep: "0".to_string(),
            id: "id-new".to_string(),
            title: "Brand New Show".to_string(),
        },
        before_b.clone(),
    ];

    let changed = detect_latest_watch_event(&before_map, &before_ordered, &after_ordered)
        .expect("new appended entry should be selected");
    assert_eq!(changed.id, "id-new");
    assert_eq!(changed.ep, "0");
}

#[test]
fn detect_latest_watch_event_returns_none_when_content_is_unchanged() {
    let before_entry = HistEntry {
        ep: "1".to_string(),
        id: "id-existing".to_string(),
        title: "Existing Show".to_string(),
    };
    let mut before_map = HashMap::new();
    before_map.insert(before_entry.id.clone(), before_entry.clone());

    let before_ordered = vec![before_entry.clone()];
    let after_ordered = vec![before_entry];

    let changed = detect_latest_watch_event(&before_map, &before_ordered, &after_ordered);
    assert!(changed.is_none());
}

#[test]
fn history_file_touched_detects_metadata_change() {
    let before = Some(HistFileSig {
        len: 100,
        modified_ns: 1000,
    });
    let after = Some(HistFileSig {
        len: 100,
        modified_ns: 1001,
    });
    assert!(history_file_touched(before, after));
}

#[test]
fn history_file_touched_ignores_same_metadata() {
    let sig = Some(HistFileSig {
        len: 100,
        modified_ns: 1000,
    });
    assert!(!history_file_touched(sig, sig));
}

#[test]
fn added_entries_detects_inserted_and_duplicate_new_occurrences() {
    let before = vec![
        HistEntry {
            ep: "1".to_string(),
            id: "a".to_string(),
            title: "A".to_string(),
        },
        HistEntry {
            ep: "2".to_string(),
            id: "b".to_string(),
            title: "B".to_string(),
        },
    ];
    let after = vec![
        before[0].clone(),
        HistEntry {
            ep: "0".to_string(),
            id: "c".to_string(),
            title: "C".to_string(),
        },
        before[1].clone(),
        HistEntry {
            ep: "2".to_string(),
            id: "b".to_string(),
            title: "B".to_string(),
        },
    ];

    let added = added_entries(&before, &after);
    assert_eq!(added.len(), 2);
    assert_eq!(added[0].id, "c");
    assert_eq!(added[1].id, "b");
}

#[test]
fn parse_journal_ani_cli_line_extracts_timestamp_and_message() {
    let line = "1772039324.974245 fedora ani-cli[407433]: Shingeki no Kyojin 0";
    let (ts_ns, msg) = parse_journal_ani_cli_line(line).expect("line should parse");
    assert_eq!(msg, "Shingeki no Kyojin 0");
    assert_eq!(ts_ns, 1_772_039_324_974_245_000);
}

#[test]
fn ani_cli_log_key_matches_ani_cli_logger_format() {
    let key = ani_cli_log_key("Death Note: Rewrite (1 episodes)", "1");
    assert_eq!(key, "Death Note Rewrite 1");
}

#[test]
fn ani_cli_log_key_normalizes_missing_space_before_parentheses() {
    let key = ani_cli_log_key("Naruto(220 episodes)", "1");
    assert_eq!(key, "Naruto 1");
}

#[test]
fn detect_log_matched_entry_handles_episode_zero() {
    let after_ordered = vec![
        HistEntry {
            ep: "1".to_string(),
            id: "id-1".to_string(),
            title: "Death Note (37 episodes)".to_string(),
        },
        HistEntry {
            ep: "0".to_string(),
            id: "id-2".to_string(),
            title: "Shingeki no Kyojin (27 episodes)".to_string(),
        },
    ];

    let matched = detect_log_matched_entry("Shingeki no Kyojin 0", &after_ordered)
        .expect("message should map to history entry");
    assert_eq!(matched.id, "id-2");
    assert_eq!(matched.ep, "0");
}

#[test]
fn episode_ordinal_from_list_counts_zero_and_decimal_entries() {
    let mut episodes = vec!["0".to_string()];
    for ep in 1..=13 {
        episodes.push(ep.to_string());
    }
    episodes.push("13.5".to_string());
    for ep in 14..=25 {
        episodes.push(ep.to_string());
    }

    let ordinal =
        episode_ordinal_from_list("25", &episodes).expect("episode should be found in list");
    assert_eq!(ordinal, 27);
}

#[test]
fn build_progress_gauge_uses_episode_ordinal_when_list_available() {
    let mut episodes = vec!["0".to_string()];
    for ep in 1..=13 {
        episodes.push(ep.to_string());
    }
    episodes.push("13.5".to_string());
    for ep in 14..=25 {
        episodes.push(ep.to_string());
    }

    let (ratio, label) =
        build_progress_gauge("25", 27, Some(&episodes)).expect("gauge should be generated");
    assert!((ratio - 1.0).abs() < 0.000_001);
    assert_eq!(label, "27/27");
}

#[test]
fn build_progress_gauge_falls_back_to_numeric_episode_without_list() {
    let (ratio, label) =
        build_progress_gauge("25", 27, None).expect("numeric fallback should work");
    assert!((ratio - (25.0 / 27.0)).abs() < 0.000_001);
    assert_eq!(label, "25/27");
}

#[test]
fn format_episode_progress_text_uses_ordinal_and_keeps_raw_label_when_needed() {
    let mut episodes = vec!["0".to_string()];
    for ep in 1..=13 {
        episodes.push(ep.to_string());
    }
    episodes.push("13.5".to_string());
    for ep in 14..=25 {
        episodes.push(ep.to_string());
    }

    let text = format_episode_progress_text("25", 27, Some(&episodes));
    assert_eq!(text, "27 of 27 (episode 25)");
}

#[test]
fn format_episode_progress_text_uses_plain_numeric_when_ordinal_matches() {
    let text = format_episode_progress_text("12", 24, None);
    assert_eq!(text, "12 of 24");
}

#[test]
fn replay_seed_episode_uses_previous_episode_from_list() {
    let episodes = vec![
        "0".to_string(),
        "1".to_string(),
        "2".to_string(),
        "13".to_string(),
        "13.5".to_string(),
    ];

    let seed = replay_seed_episode("13.5", Some(&episodes));
    assert_eq!(seed.as_deref(), Some("13"));
}

#[test]
fn replay_seed_episode_none_for_first_episode_in_list() {
    let episodes = vec!["0".to_string(), "1".to_string(), "2".to_string()];
    let seed = replay_seed_episode("0", Some(&episodes));
    assert!(seed.is_none());
}

#[test]
fn replay_seed_episode_falls_back_to_numeric_when_list_missing() {
    let seed = replay_seed_episode("5", None);
    assert_eq!(seed.as_deref(), Some("4"));

    let seed_first = replay_seed_episode("1", None);
    assert!(seed_first.is_none());
}

#[test]
fn replay_plan_uses_select_nth_for_episode_zero_fallback() {
    let item = crate::db::SeenEntry {
        ani_id: "show-0".to_string(),
        title: "Replay Zero Show (2 episodes)".to_string(),
        last_episode: "0".to_string(),
        last_seen_at: "2026-02-27T00:00:00+00:00".to_string(),
    };
    let episodes = vec!["0".to_string(), "1".to_string(), "2".to_string()];

    let plan = build_replay_plan(&item, Some(&episodes), |_| Some(4));
    assert_eq!(
        plan,
        ReplayPlan::Episode {
            episode: "0".to_string(),
            select_nth: Some(4),
        }
    );
}

#[test]
fn replay_plan_uses_continue_seed_when_available() {
    let item = crate::db::SeenEntry {
        ani_id: "show-5".to_string(),
        title: "Replay Normal Show (12 episodes)".to_string(),
        last_episode: "5".to_string(),
        last_seen_at: "2026-02-27T00:00:00+00:00".to_string(),
    };

    let plan = build_replay_plan(&item, None, |_| Some(99));
    assert_eq!(
        plan,
        ReplayPlan::Continue {
            seed_episode: "4".to_string(),
        }
    );
}

#[test]
fn previous_target_episode_uses_episode_list_for_non_linear_numbering() {
    let episodes = vec![
        "0".to_string(),
        "1".to_string(),
        "2".to_string(),
        "13".to_string(),
        "13.5".to_string(),
    ];
    let previous = previous_target_episode("13.5", Some(&episodes));
    assert_eq!(previous.as_deref(), Some("13"));
}

#[test]
fn previous_seed_episode_steps_back_two_positions_when_possible() {
    let episodes = vec![
        "0".to_string(),
        "1".to_string(),
        "2".to_string(),
        "13".to_string(),
        "13.5".to_string(),
    ];
    let seed = previous_seed_episode("13.5", Some(&episodes));
    assert_eq!(seed.as_deref(), Some("2"));
}

#[test]
fn previous_episode_helpers_fall_back_to_numeric() {
    assert_eq!(previous_target_episode("5", None).as_deref(), Some("4"));
    assert_eq!(previous_seed_episode("5", None).as_deref(), Some("3"));
    assert_eq!(previous_target_episode("1", None).as_deref(), Some("0"));
    assert_eq!(previous_target_episode("0", None).as_deref(), None);
    assert_eq!(previous_seed_episode("2", None).as_deref(), None);
}

#[test]
fn has_previous_episode_handles_decimal_and_zero_when_list_missing() {
    assert!(has_previous_episode("13.5", None));
    assert!(has_previous_episode("1", None));
    assert!(!has_previous_episode("0", None));
}

#[test]
fn parse_search_result_entries_extracts_ids_in_order() {
    let raw =
        r#"{"data":{"shows":{"edges":[{"_id":"id-1","name":"A"},{"_id":"id-2","name":"B"}]}}}"#;
    let entries = parse_search_result_entries(raw);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].id, "id-1");
    assert_eq!(entries[0].title, "A");
    assert_eq!(entries[1].id, "id-2");
    assert_eq!(entries[1].title, "B");
}

#[test]
fn find_select_nth_index_by_id_returns_one_based_position() {
    let entries = vec![
        SearchResultEntry {
            id: "id-1".to_string(),
            title: "A".to_string(),
        },
        SearchResultEntry {
            id: "id-2".to_string(),
            title: "B".to_string(),
        },
        SearchResultEntry {
            id: "id-3".to_string(),
            title: "C".to_string(),
        },
    ];
    assert_eq!(find_select_nth_index_by_id(&entries, "id-2"), Some(2));
    assert_eq!(find_select_nth_index_by_id(&entries, "id-missing"), None);
}

#[test]
fn find_select_nth_index_by_title_matches_normalized_title() {
    let entries = vec![
        SearchResultEntry {
            id: "id-1".to_string(),
            title: "Shingeki no Kyojin".to_string(),
        },
        SearchResultEntry {
            id: "id-2".to_string(),
            title: "Death Note".to_string(),
        },
    ];
    assert_eq!(
        find_select_nth_index_by_title(&entries, "Shingeki no Kyojin (27 episodes)"),
        Some(1)
    );
}

#[test]
fn json_escape_handles_quotes_backslashes_and_controls() {
    let escaped = json_escape("A\"B\\C\n");
    assert_eq!(escaped, "A\\\"B\\\\C\\n");
}

#[test]
fn previous_episode_helpers_support_decimal_fallback_without_list() {
    assert_eq!(previous_target_episode("15.5", None).as_deref(), Some("15"));
    assert_eq!(previous_seed_episode("15.5", None).as_deref(), Some("14"));
}

#[test]
fn tui_action_horizontal_navigation_respects_edges() {
    assert_eq!(TuiAction::Next.move_left(), TuiAction::Next);
    assert_eq!(TuiAction::Next.move_right(), TuiAction::Replay);
    assert_eq!(TuiAction::Replay.move_right(), TuiAction::Previous);
    assert_eq!(TuiAction::Previous.move_right(), TuiAction::Select);
    assert_eq!(TuiAction::Select.move_right(), TuiAction::Select);
    assert_eq!(TuiAction::Select.move_left(), TuiAction::Previous);
}

#[test]
fn has_next_episode_uses_episode_list_for_non_linear_numbering() {
    let mut episodes = vec!["0".to_string()];
    for ep in 1..=13 {
        episodes.push(ep.to_string());
    }
    episodes.push("13.5".to_string());
    for ep in 14..=25 {
        episodes.push(ep.to_string());
    }

    assert!(!has_next_episode("25", Some(27), Some(&episodes)));
    assert!(has_next_episode("24", Some(27), Some(&episodes)));
}

#[test]
fn has_next_episode_falls_back_to_numeric_when_list_missing() {
    assert!(has_next_episode("25", Some(27), None));
    assert!(!has_next_episode("27", Some(27), None));
}

#[test]
fn format_last_seen_display_parses_rfc3339_timestamp() {
    let raw = "2026-02-25T18:27:06.100701256+00:00";
    let formatted = format_last_seen_display(raw);
    let expected = DateTime::parse_from_rfc3339(raw)
        .expect("timestamp should parse")
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M %:z")
        .to_string();
    assert_eq!(formatted, expected);
}

#[test]
fn format_last_seen_display_tui_parses_rfc3339_timestamp_without_offset() {
    let raw = "2026-02-25T18:27:06.100701256+00:00";
    let formatted = format_last_seen_display_tui(raw);
    let expected = DateTime::parse_from_rfc3339(raw)
        .expect("timestamp should parse")
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string();
    assert_eq!(formatted, expected);
}

#[test]
fn format_last_seen_display_keeps_raw_when_invalid() {
    let raw = "not-a-timestamp";
    assert_eq!(format_last_seen_display(raw), raw);
}

#[test]
fn format_last_seen_display_tui_keeps_raw_when_invalid() {
    let raw = "not-a-timestamp";
    assert_eq!(format_last_seen_display_tui(raw), raw);
}

#[test]
fn resolve_ani_cli_bin_from_env_uses_override_when_present() {
    let resolved = resolve_ani_cli_bin_from_env(Some(OsString::from("/tmp/fake-ani-cli")));
    assert_eq!(resolved, std::path::PathBuf::from("/tmp/fake-ani-cli"));
}

#[test]
fn resolve_ani_cli_bin_from_env_falls_back_on_missing_or_empty() {
    let missing = resolve_ani_cli_bin_from_env(None);
    assert_eq!(missing, std::path::PathBuf::from("ani-cli"));

    let empty = resolve_ani_cli_bin_from_env(Some(OsString::new()));
    assert_eq!(empty, std::path::PathBuf::from("ani-cli"));
}

#[test]
fn temp_hist_dir_drop_removes_directory() {
    let temp_hist_dir = TempHistDir::new().expect("temp history dir should be created");
    let temp_path = temp_hist_dir.path().to_path_buf();
    assert!(
        temp_path.exists(),
        "temp history dir should exist while guard is alive"
    );
    drop(temp_hist_dir);
    assert!(
        !temp_path.exists(),
        "temp history dir should be removed when guard is dropped"
    );
}

#[test]
fn parse_mode_episode_labels_extracts_string_and_numeric_values() {
    let payload = r#"{"data":{"show":{"availableEpisodesDetail":{"sub":[ "0","1",null,"2" ]}}}}"#;
    let episodes = parse_mode_episode_labels(payload, "sub").expect("sub episodes should parse");
    assert_eq!(episodes, vec!["0", "1", "2"]);
}

#[test]
fn parse_search_result_entries_handles_escaped_titles() {
    let raw = r#"{"data":{"shows":{"edges":[{"_id":"id-1","name":"Boku no Hero Academia: Heroes Rising \"Special\""}]}}}"#;
    let entries = parse_search_result_entries(raw);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "id-1");
    assert_eq!(
        entries[0].title,
        "Boku no Hero Academia: Heroes Rising \"Special\""
    );
}

#[test]
fn parse_search_result_entries_returns_empty_on_invalid_json() {
    let entries = parse_search_result_entries("{not json");
    assert!(entries.is_empty());
}

#[cfg(any(unix, windows))]
static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(any(unix, windows))]
fn env_lock() -> &'static Mutex<()> {
    ENV_LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(any(unix, windows))]
fn env_lock_guard() -> std::sync::MutexGuard<'static, ()> {
    env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(any(unix, windows))]
#[derive(Debug)]
struct TestSandbox {
    root: PathBuf,
}

#[cfg(any(unix, windows))]
impl TestSandbox {
    fn new(prefix: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "anitrack-integration-{prefix}-{}-{}",
            std::process::id(),
            unix_now_ns()
        ));
        fs::create_dir_all(&root).expect("test sandbox should be created");
        Self { root }
    }
}

#[cfg(any(unix, windows))]
impl Drop for TestSandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(any(unix, windows))]
struct ScopedEnvVar {
    key: String,
    previous: Option<OsString>,
}

#[cfg(any(unix, windows))]
impl ScopedEnvVar {
    fn set(key: &str, value: &OsStr) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self {
            key: key.to_string(),
            previous,
        }
    }
}

#[cfg(any(unix, windows))]
impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        match self.previous.as_ref() {
            Some(prev) => unsafe {
                std::env::set_var(&self.key, prev);
            },
            None => unsafe {
                std::env::remove_var(&self.key);
            },
        }
    }
}

#[cfg(any(unix, windows))]
fn open_test_db(root: &Path) -> Database {
    let db = Database::open(&root.join("anitrack.db")).expect("test db should open");
    db.migrate().expect("test db migration should succeed");
    db
}

#[cfg(unix)]
fn create_fake_ani_cli(root: &Path) -> PathBuf {
    let script_path = root.join("fake-ani-cli.sh");
    let script = r#"#!/usr/bin/env bash
set -euo pipefail

mode="${ANITRACK_FAKE_MODE:-}"
hist_dir="${ANI_CLI_HIST_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/ani-cli}"
hist_file="${hist_dir}/ani-hsts"
mkdir -p "${hist_dir}"

case "${mode}" in
  start_success)
    printf '1\tshow-1\tShow One\n' >> "${hist_file}"
    ;;
  replay_success|next_success|previous_success)
    line="$(tail -n 1 "${hist_file}" 2>/dev/null || true)"
    if [ -n "${line}" ]; then
      IFS=$'\t' read -r ep ani_id title <<< "${line}"
      next_ep=$((ep + 1))
      printf '%s\t%s\t%s\n' "${next_ep}" "${ani_id}" "${title}" > "${hist_file}"
    fi
    ;;
  select_success)
    ani_id="${ANITRACK_FAKE_ANI_ID:-show-1}"
    title="${ANITRACK_FAKE_TITLE:-Show One}"
    episode="${ANITRACK_FAKE_EPISODE:-2}"
    printf '%s\t%s\t%s\n' "${episode}" "${ani_id}" "${title}" > "${hist_file}"
    ;;
  next_fail|previous_fail)
    exit 1
    ;;
esac
"#;
    fs::write(&script_path, script).expect("fake ani-cli should be written");
    let mut perms = fs::metadata(&script_path)
        .expect("fake script metadata should exist")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("fake ani-cli should be executable");
    script_path
}

#[cfg(unix)]
#[test]
fn integration_start_records_watch_progress_with_fake_ani_cli() {
    let _env_guard = env_lock_guard();
    let sandbox = TestSandbox::new("start");
    let db = open_test_db(&sandbox.root);
    let fake_ani_cli = create_fake_ani_cli(&sandbox.root);
    let hist_dir = sandbox.root.join("hist");
    fs::create_dir_all(&hist_dir).expect("hist directory should be created");

    let _bin = ScopedEnvVar::set("ANI_TRACK_ANI_CLI_BIN", fake_ani_cli.as_os_str());
    let _hist = ScopedEnvVar::set("ANI_CLI_HIST_DIR", hist_dir.as_os_str());
    let _mode = ScopedEnvVar::set("ANITRACK_FAKE_MODE", OsStr::new("start_success"));

    run_start(&db).expect("start command should succeed");

    let last_seen = db
        .last_seen()
        .expect("db query should succeed")
        .expect("entry should be recorded");
    assert_eq!(last_seen.ani_id, "show-1");
    assert_eq!(last_seen.title, "Show One");
    assert_eq!(last_seen.last_episode, "1");
}

#[cfg(unix)]
#[test]
fn integration_next_updates_progress_when_fake_continue_succeeds() {
    let _env_guard = env_lock_guard();
    let sandbox = TestSandbox::new("next-success");
    let db = open_test_db(&sandbox.root);
    let fake_ani_cli = create_fake_ani_cli(&sandbox.root);
    db.upsert_seen("show-1", "Show One", "1")
        .expect("seed row should be inserted");

    let _bin = ScopedEnvVar::set("ANI_TRACK_ANI_CLI_BIN", fake_ani_cli.as_os_str());
    let _mode = ScopedEnvVar::set("ANITRACK_FAKE_MODE", OsStr::new("next_success"));

    run_next(&db).expect("next command should complete");

    let last_seen = db
        .last_seen()
        .expect("db query should succeed")
        .expect("entry should exist");
    assert_eq!(last_seen.last_episode, "2");
}

#[cfg(unix)]
#[test]
fn integration_next_keeps_progress_when_fake_continue_fails() {
    let _env_guard = env_lock_guard();
    let sandbox = TestSandbox::new("next-fail");
    let db = open_test_db(&sandbox.root);
    let fake_ani_cli = create_fake_ani_cli(&sandbox.root);
    db.upsert_seen("show-1", "Show One", "1")
        .expect("seed row should be inserted");

    let _bin = ScopedEnvVar::set("ANI_TRACK_ANI_CLI_BIN", fake_ani_cli.as_os_str());
    let _mode = ScopedEnvVar::set("ANITRACK_FAKE_MODE", OsStr::new("next_fail"));

    run_next(&db).expect("next command should not bubble fake failure");

    let last_seen = db
        .last_seen()
        .expect("db query should succeed")
        .expect("entry should exist");
    assert_eq!(last_seen.last_episode, "1");
}

#[cfg(unix)]
#[test]
fn integration_replay_updates_progress_with_fake_continue() {
    let _env_guard = env_lock_guard();
    let sandbox = TestSandbox::new("replay-success");
    let db = open_test_db(&sandbox.root);
    let fake_ani_cli = create_fake_ani_cli(&sandbox.root);
    db.upsert_seen("show-1", "Show One", "2")
        .expect("seed row should be inserted");

    let _bin = ScopedEnvVar::set("ANI_TRACK_ANI_CLI_BIN", fake_ani_cli.as_os_str());
    let _mode = ScopedEnvVar::set("ANITRACK_FAKE_MODE", OsStr::new("replay_success"));

    run_replay(&db).expect("replay command should complete");

    let last_seen = db
        .last_seen()
        .expect("db query should succeed")
        .expect("entry should exist");
    assert_eq!(last_seen.last_episode, "2");
}

#[cfg(unix)]
#[test]
fn integration_select_updates_progress_with_override_without_network() {
    let _env_guard = env_lock_guard();
    let sandbox = TestSandbox::new("select-success");
    let db = open_test_db(&sandbox.root);
    let fake_ani_cli = create_fake_ani_cli(&sandbox.root);
    let hist_dir = sandbox.root.join("hist");
    fs::create_dir_all(&hist_dir).expect("hist directory should be created");
    fs::write(hist_dir.join("ani-hsts"), "1\tshow-1\tShow One\n")
        .expect("initial history should be seeded");
    db.upsert_seen("show-1", "Show One", "1")
        .expect("seed row should be inserted");
    let item = db
        .last_seen()
        .expect("db query should succeed")
        .expect("entry should exist");

    let _bin = ScopedEnvVar::set("ANI_TRACK_ANI_CLI_BIN", fake_ani_cli.as_os_str());
    let _hist = ScopedEnvVar::set("ANI_CLI_HIST_DIR", hist_dir.as_os_str());
    let _mode = ScopedEnvVar::set("ANITRACK_FAKE_MODE", OsStr::new("select_success"));
    let _select_override = ScopedEnvVar::set("ANI_TRACK_TEST_SELECT_NTH", OsStr::new("1"));
    let _select_id = ScopedEnvVar::set("ANITRACK_FAKE_ANI_ID", OsStr::new("show-1"));
    let _select_title = ScopedEnvVar::set("ANITRACK_FAKE_TITLE", OsStr::new("Show One"));
    let _select_episode = ScopedEnvVar::set("ANITRACK_FAKE_EPISODE", OsStr::new("2"));

    let outcome = run_ani_cli_select(&item).expect("select action should run");
    assert!(outcome.success, "select action should report success");
    let updated_ep = outcome
        .final_episode
        .expect("updated episode should be detected");
    db.upsert_seen(&item.ani_id, &item.title, &updated_ep)
        .expect("db update should succeed");

    let last_seen = db
        .last_seen()
        .expect("db query should succeed")
        .expect("entry should exist");
    assert_eq!(last_seen.last_episode, "2");
}

#[cfg(unix)]
#[test]
fn integration_previous_updates_progress_when_fake_continue_succeeds() {
    let _env_guard = env_lock_guard();
    let sandbox = TestSandbox::new("previous-success");
    let db = open_test_db(&sandbox.root);
    let fake_ani_cli = create_fake_ani_cli(&sandbox.root);
    let item = crate::db::SeenEntry {
        ani_id: "show-1".to_string(),
        title: "Show One".to_string(),
        last_episode: "3".to_string(),
        last_seen_at: "2026-02-27T00:00:00+00:00".to_string(),
    };
    let episodes = vec!["1".to_string(), "2".to_string(), "3".to_string()];

    let _bin = ScopedEnvVar::set("ANI_TRACK_ANI_CLI_BIN", fake_ani_cli.as_os_str());
    let _mode = ScopedEnvVar::set("ANITRACK_FAKE_MODE", OsStr::new("previous_success"));

    let outcome = run_ani_cli_previous(&item, Some(&episodes)).expect("previous action should run");
    assert!(outcome.success, "previous action should report success");
    let updated_ep = outcome
        .final_episode
        .expect("updated episode should be detected");

    db.upsert_seen(&item.ani_id, &item.title, &updated_ep)
        .expect("db update should succeed");
    let last_seen = db
        .last_seen()
        .expect("db query should succeed")
        .expect("entry should exist");
    assert_eq!(last_seen.last_episode, "2");
}

#[cfg(unix)]
#[test]
fn integration_previous_keeps_progress_when_no_previous_available() {
    let _env_guard = env_lock_guard();
    let sandbox = TestSandbox::new("previous-noop");
    let db = open_test_db(&sandbox.root);
    db.upsert_seen("show-1", "Show One", "0")
        .expect("seed row should be inserted");
    let item = db
        .last_seen()
        .expect("db query should succeed")
        .expect("entry should exist");
    let episodes = vec!["0".to_string(), "1".to_string(), "2".to_string()];

    let err =
        run_ani_cli_previous(&item, Some(&episodes)).expect_err("no previous should return error");
    assert!(
        err.to_string().contains("no previous episode available"),
        "unexpected error: {err}"
    );

    let last_seen = db
        .last_seen()
        .expect("db query should succeed")
        .expect("entry should exist");
    assert_eq!(last_seen.last_episode, "0");
}

#[cfg(unix)]
#[test]
fn integration_previous_keeps_progress_when_playback_fails() {
    let _env_guard = env_lock_guard();
    let sandbox = TestSandbox::new("previous-fail");
    let db = open_test_db(&sandbox.root);
    let fake_ani_cli = create_fake_ani_cli(&sandbox.root);
    db.upsert_seen("show-1", "Show One", "3")
        .expect("seed row should be inserted");
    let item = db
        .last_seen()
        .expect("db query should succeed")
        .expect("entry should exist");
    let episodes = vec!["1".to_string(), "2".to_string(), "3".to_string()];

    let _bin = ScopedEnvVar::set("ANI_TRACK_ANI_CLI_BIN", fake_ani_cli.as_os_str());
    let _mode = ScopedEnvVar::set("ANITRACK_FAKE_MODE", OsStr::new("previous_fail"));

    let outcome = run_ani_cli_previous(&item, Some(&episodes)).expect("previous action should run");
    assert!(!outcome.success, "previous action should report failure");
    assert!(outcome.final_episode.is_none());
    assert!(
        outcome
            .failure_detail
            .as_deref()
            .unwrap_or_default()
            .contains("possible network outage or interrupted playback"),
        "failure detail should include actionable hint: {:?}",
        outcome.failure_detail
    );

    let last_seen = db
        .last_seen()
        .expect("db query should succeed")
        .expect("entry should exist");
    assert_eq!(last_seen.last_episode, "3");
}

#[cfg(windows)]
fn create_fake_ani_cli(root: &Path) -> PathBuf {
    let cmd_path = root.join("fake-ani-cli.cmd");
    let ps1_path = root.join("fake-ani-cli.ps1");
    let cmd_script = "@echo off\r\npowershell -NoProfile -ExecutionPolicy Bypass -File \"%~dp0fake-ani-cli.ps1\"\r\nexit /b %ERRORLEVEL%\r\n";
    let ps1_script = r#"
$mode = $env:ANITRACK_FAKE_MODE
if ($env:ANI_CLI_HIST_DIR) {
  $histDir = $env:ANI_CLI_HIST_DIR
} elseif ($env:XDG_STATE_HOME) {
  $histDir = Join-Path $env:XDG_STATE_HOME "ani-cli"
} else {
  $histDir = Join-Path $env:USERPROFILE ".local\state\ani-cli"
}
$histFile = Join-Path $histDir "ani-hsts"
New-Item -ItemType Directory -Path $histDir -Force | Out-Null

function Bump-History {
  if (-not (Test-Path $histFile)) { return }
  $line = Get-Content -Path $histFile | Select-Object -Last 1
  if (-not $line) { return }
  $parts = $line -split "`t", 3
  if ($parts.Length -lt 3) { return }
  $nextEp = ([int]$parts[0]) + 1
  Set-Content -Path $histFile -Value ("{0}`t{1}`t{2}" -f $nextEp, $parts[1], $parts[2])
}

switch ($mode) {
  "start_success" {
    Add-Content -Path $histFile -Value "1`tshow-1`tShow One"
    exit 0
  }
  "replay_success" { Bump-History; exit 0 }
  "next_success" { Bump-History; exit 0 }
  "previous_success" { Bump-History; exit 0 }
  "select_success" {
    $aniId = if ($env:ANITRACK_FAKE_ANI_ID) { $env:ANITRACK_FAKE_ANI_ID } else { "show-1" }
    $title = if ($env:ANITRACK_FAKE_TITLE) { $env:ANITRACK_FAKE_TITLE } else { "Show One" }
    $episode = if ($env:ANITRACK_FAKE_EPISODE) { $env:ANITRACK_FAKE_EPISODE } else { "2" }
    Set-Content -Path $histFile -Value ("{0}`t{1}`t{2}" -f $episode, $aniId, $title)
    exit 0
  }
  "next_fail" { exit 1 }
  "previous_fail" { exit 1 }
  default { exit 0 }
}
"#;
    fs::write(&cmd_path, cmd_script).expect("fake ani-cli cmd wrapper should be written");
    fs::write(&ps1_path, ps1_script).expect("fake ani-cli powershell should be written");
    cmd_path
}

#[cfg(windows)]
#[test]
fn integration_start_records_watch_progress_with_fake_ani_cli_windows() {
    let _env_guard = env_lock_guard();
    let sandbox = TestSandbox::new("start-win");
    let db = open_test_db(&sandbox.root);
    let fake_ani_cli = create_fake_ani_cli(&sandbox.root);
    let hist_dir = sandbox.root.join("hist");
    fs::create_dir_all(&hist_dir).expect("hist directory should be created");

    let _bin = ScopedEnvVar::set("ANI_TRACK_ANI_CLI_BIN", fake_ani_cli.as_os_str());
    let _hist = ScopedEnvVar::set("ANI_CLI_HIST_DIR", hist_dir.as_os_str());
    let _mode = ScopedEnvVar::set("ANITRACK_FAKE_MODE", OsStr::new("start_success"));

    run_start(&db).expect("start command should succeed");

    let last_seen = db
        .last_seen()
        .expect("db query should succeed")
        .expect("entry should be recorded");
    assert_eq!(last_seen.ani_id, "show-1");
    assert_eq!(last_seen.title, "Show One");
    assert_eq!(last_seen.last_episode, "1");
}

#[cfg(windows)]
#[test]
fn integration_next_updates_progress_when_fake_continue_succeeds_windows() {
    let _env_guard = env_lock_guard();
    let sandbox = TestSandbox::new("next-success-win");
    let db = open_test_db(&sandbox.root);
    let fake_ani_cli = create_fake_ani_cli(&sandbox.root);
    db.upsert_seen("show-1", "Show One", "1")
        .expect("seed row should be inserted");

    let _bin = ScopedEnvVar::set("ANI_TRACK_ANI_CLI_BIN", fake_ani_cli.as_os_str());
    let _mode = ScopedEnvVar::set("ANITRACK_FAKE_MODE", OsStr::new("next_success"));

    run_next(&db).expect("next command should complete");

    let last_seen = db
        .last_seen()
        .expect("db query should succeed")
        .expect("entry should exist");
    assert_eq!(last_seen.last_episode, "2");
}

#[cfg(windows)]
#[test]
fn integration_previous_reports_failure_detail_when_playback_fails_windows() {
    let _env_guard = env_lock_guard();
    let sandbox = TestSandbox::new("previous-fail-win");
    let db = open_test_db(&sandbox.root);
    let fake_ani_cli = create_fake_ani_cli(&sandbox.root);
    db.upsert_seen("show-1", "Show One", "3")
        .expect("seed row should be inserted");
    let item = db
        .last_seen()
        .expect("db query should succeed")
        .expect("entry should exist");
    let episodes = vec!["1".to_string(), "2".to_string(), "3".to_string()];

    let _bin = ScopedEnvVar::set("ANI_TRACK_ANI_CLI_BIN", fake_ani_cli.as_os_str());
    let _mode = ScopedEnvVar::set("ANITRACK_FAKE_MODE", OsStr::new("previous_fail"));

    let outcome = run_ani_cli_previous(&item, Some(&episodes)).expect("previous action should run");
    assert!(!outcome.success, "previous action should report failure");
    assert!(outcome.final_episode.is_none());
    assert!(
        outcome
            .failure_detail
            .as_deref()
            .unwrap_or_default()
            .contains("possible network outage or interrupted playback"),
        "failure detail should include actionable hint: {:?}",
        outcome.failure_detail
    );

    let last_seen = db
        .last_seen()
        .expect("db query should succeed")
        .expect("entry should exist");
    assert_eq!(last_seen.last_episode, "3");
}
