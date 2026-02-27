use std::collections::HashMap;

use super::episode::*;
use super::tracking::*;
use super::tui::TuiAction;

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
    let formatted = format_last_seen_display("2026-02-25T18:27:06.100701256+00:00");
    assert_eq!(formatted, "2026-02-25 18:27");
}

#[test]
fn format_last_seen_display_keeps_raw_when_invalid() {
    let raw = "not-a-timestamp";
    assert_eq!(format_last_seen_display(raw), raw);
}
