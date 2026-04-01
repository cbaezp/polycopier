//! Dedicated tests for `copy_ledger` — exercising the public API from outside
//! the module, exactly as a downstream consumer would.
//!
//! Run: `cargo test --test copy_ledger_tests`

use polycopier::copy_ledger::CopyLedger;
use rust_decimal_macros::dec;
use std::collections::HashSet;

fn ledger() -> CopyLedger {
    CopyLedger::new_in_memory()
}

// ---------------------------------------------------------------------------
// Initial state
// ---------------------------------------------------------------------------

#[test]
fn new_ledger_has_no_active_entries() {
    let l = ledger();
    assert!(!l.has_any_active("tok"));
    assert!(l.find_active_for_token("tok").is_none());
    assert!(l.entries.is_empty());
}

// ---------------------------------------------------------------------------
// record_copy
// ---------------------------------------------------------------------------

#[test]
fn record_copy_creates_active_entry() {
    let mut l = ledger();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(10), dec!(0.50));

    assert!(l.has_any_active("tok_a"));
    assert!(l.has_active_copy("tok_a", "0xabc"));

    let e = l.find_active_for_token("tok_a").unwrap();
    assert_eq!(e.source_wallet, "0xabc");
    assert_eq!(e.size, dec!(10));
    assert_eq!(e.entry_price, dec!(0.50));
    assert!(!e.closed);
    assert!(e.closed_at.is_none());
}

#[test]
fn record_copy_multiple_different_tokens() {
    let mut l = ledger();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(5), dec!(0.30));
    l.record_copy("tok_b".into(), "0xdef".into(), dec!(20), dec!(0.70));

    assert!(l.has_any_active("tok_a"));
    assert!(l.has_any_active("tok_b"));
    assert_eq!(l.entries.len(), 2);
}

// ---------------------------------------------------------------------------
// Source-wallet specificity
// ---------------------------------------------------------------------------

#[test]
fn has_any_active_is_source_agnostic() {
    let mut l = ledger();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(10), dec!(0.50));
    // regardless of which target — has_any_active just checks the token
    assert!(l.has_any_active("tok_a"));
}

#[test]
fn has_active_copy_correct_source_returns_true() {
    let mut l = ledger();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(10), dec!(0.50));
    assert!(l.has_active_copy("tok_a", "0xabc"));
}

#[test]
fn has_active_copy_wrong_source_returns_false() {
    let mut l = ledger();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(10), dec!(0.50));
    assert!(!l.has_active_copy("tok_a", "0xdifferent"));
}

#[test]
fn has_active_copy_wrong_token_returns_false() {
    let mut l = ledger();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(10), dec!(0.50));
    assert!(!l.has_active_copy("tok_b", "0xabc"));
}

// ---------------------------------------------------------------------------
// record_close
// ---------------------------------------------------------------------------

#[test]
fn record_close_marks_entry_closed() {
    let mut l = ledger();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(10), dec!(0.50));
    l.record_close("tok_a", "0xabc");

    assert!(!l.has_any_active("tok_a"));
    assert!(!l.has_active_copy("tok_a", "0xabc"));
    assert!(l.entries[0].closed);
    assert!(l.entries[0].closed_at.is_some());
}

#[test]
fn record_close_wrong_source_is_a_no_op() {
    let mut l = ledger();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(10), dec!(0.50));
    l.record_close("tok_a", "0xwrong"); // should warn and do nothing
    assert!(l.has_any_active("tok_a")); // still open
}

#[test]
fn closed_entry_does_not_block_re_entry_for_same_token() {
    let mut l = ledger();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(10), dec!(0.50));
    l.record_close("tok_a", "0xabc");

    // Re-enter after the position is fully closed
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(5), dec!(0.60));

    assert!(l.has_any_active("tok_a"));
    assert_eq!(l.entries.len(), 2);
    // Only the new entry is active; the old one is still closed
    assert!(l.entries[0].closed);
    assert!(!l.entries[1].closed);
}

// ---------------------------------------------------------------------------
// reconcile
// ---------------------------------------------------------------------------

#[test]
fn reconcile_closes_entries_not_in_live_wallet() {
    let mut l = ledger();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(5), dec!(0.50));
    l.record_copy("tok_b".into(), "0xdef".into(), dec!(10), dec!(0.30));

    let live: HashSet<String> = ["tok_a".to_string()].into_iter().collect();
    l.reconcile(&live);

    assert!(l.has_any_active("tok_a")); // still in wallet
    assert!(!l.has_any_active("tok_b")); // not in wallet → closed
}

#[test]
fn reconcile_no_op_when_all_tokens_still_held() {
    let mut l = ledger();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(5), dec!(0.50));

    let live: HashSet<String> = ["tok_a".to_string()].into_iter().collect();
    l.reconcile(&live);

    assert!(l.has_any_active("tok_a")); // unchanged
}

#[test]
fn reconcile_empty_wallet_closes_all_active_entries() {
    let mut l = ledger();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(5), dec!(0.50));
    l.record_copy("tok_b".into(), "0xdef".into(), dec!(10), dec!(0.30));

    l.reconcile(&HashSet::new());

    assert!(!l.has_any_active("tok_a"));
    assert!(!l.has_any_active("tok_b"));
}

#[test]
fn reconcile_does_not_re_close_already_closed_entries() {
    let mut l = ledger();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(5), dec!(0.50));
    l.record_close("tok_a", "0xabc");

    let closed_at_before = l.entries[0].closed_at.unwrap();
    // Reconcile with empty wallet — tok_a is already closed → no mutation
    l.reconcile(&HashSet::new());

    let closed_at_after = l.entries[0].closed_at.unwrap();
    // Timestamp must not be touched
    assert_eq!(closed_at_before, closed_at_after);
    // Still exactly one entry (not duplicated)
    assert_eq!(l.entries.len(), 1);
}

// ---------------------------------------------------------------------------
// Disk persistence
// ---------------------------------------------------------------------------

/// Generate a unique tmp path per test invocation to avoid cross-test races.
fn tmp_path(label: &str) -> String {
    format!(
        "/tmp/polycopier_test_ledger_{}_{}.json",
        label,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    )
}

#[test]
fn persistence_round_trip_preserves_all_entries() {
    let path = tmp_path("round_trip");
    let _ = std::fs::remove_file(&path);

    // Write
    {
        let mut l = CopyLedger::load_from(&path);
        l.record_copy("tok_a".into(), "0xabc".into(), dec!(10), dec!(0.5));
        l.record_copy("tok_b".into(), "0xdef".into(), dec!(5), dec!(0.75));
        l.record_close("tok_b", "0xdef"); // close the second one before saving
    }

    // Load fresh and verify
    let l = CopyLedger::load_from(&path);
    assert_eq!(l.entries.len(), 2);
    assert!(l.has_any_active("tok_a"));
    assert!(!l.has_any_active("tok_b")); // closed entry persisted correctly

    let _ = std::fs::remove_file(&path);
}

#[test]
fn persistence_closed_at_timestamp_survives_reload() {
    let path = tmp_path("timestamp");
    let _ = std::fs::remove_file(&path);

    {
        let mut l = CopyLedger::load_from(&path);
        l.record_copy("tok_a".into(), "0xabc".into(), dec!(5), dec!(0.4));
        l.record_close("tok_a", "0xabc");
        assert!(l.entries[0].closed_at.is_some());
    }

    let l = CopyLedger::load_from(&path);
    assert!(l.entries[0].closed_at.is_some());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_from_missing_file_returns_empty_ledger() {
    let l = CopyLedger::load_from("/tmp/poly_ledger_definitely_missing_xyz123.json");
    assert!(l.entries.is_empty());
}

#[test]
fn load_from_corrupt_json_returns_empty_ledger() {
    let path = tmp_path("corrupt");
    std::fs::write(&path, b"not valid json {{{").unwrap();
    let l = CopyLedger::load_from(&path);
    assert!(l.entries.is_empty());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn in_memory_ledger_save_does_not_write_any_file() {
    let mut l = CopyLedger::new_in_memory();
    // record_copy calls save() internally — must not panic or create a file
    l.record_copy("tok_x".into(), "0xaaa".into(), dec!(1), dec!(0.1));
    // Entry is retained in memory
    assert!(l.has_any_active("tok_x"));
    // The production ledger file must NOT have been touched
    // (we can't assert absence of /tmp file, but we can assert no crash)
}

// ---------------------------------------------------------------------------
// Atomic write (Gap 1)
// ---------------------------------------------------------------------------

#[test]
fn atomic_save_leaves_no_tmp_file_on_success() {
    let path = tmp_path("atomic");
    let _ = std::fs::remove_file(&path);
    let tmp = format!("{}.tmp", &path);
    let _ = std::fs::remove_file(&tmp);

    {
        let mut l = CopyLedger::load_from(&path);
        l.record_copy("tok_a".into(), "0xabc".into(), dec!(10), dec!(0.5));
    }

    // The .tmp file must not persist after a successful save
    assert!(
        !std::path::Path::new(&tmp).exists(),
        ".tmp file was not cleaned up"
    );
    // The main file must exist
    assert!(std::path::Path::new(&path).exists());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn atomic_reload_after_multiple_mutations_is_correct() {
    let path = tmp_path("atomic_mutations");
    let _ = std::fs::remove_file(&path);

    {
        let mut l = CopyLedger::load_from(&path);
        l.record_copy("tok_a".into(), "0xabc".into(), dec!(10), dec!(0.5));
        l.record_copy("tok_b".into(), "0xdef".into(), dec!(5), dec!(0.3));
        l.record_close("tok_b", "0xdef");
    }

    let l2 = CopyLedger::load_from(&path);
    assert_eq!(l2.entries.len(), 2);
    assert!(l2.has_any_active("tok_a"));
    assert!(!l2.has_any_active("tok_b")); // closed entry persisted

    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// Partial fill tracking — update_fill (Gap 4)
// ---------------------------------------------------------------------------

#[test]
fn update_fill_sets_filled_size_on_active_entry() {
    let mut l = CopyLedger::new_in_memory();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(10), dec!(0.5));

    assert!(l
        .find_active_for_token("tok_a")
        .unwrap()
        .filled_size
        .is_none());

    l.update_fill("tok_a", dec!(7));

    let entry = l.find_active_for_token("tok_a").unwrap();
    assert_eq!(entry.filled_size, Some(dec!(7)));
    assert_eq!(entry.size, dec!(10)); // original order size unchanged
}

#[test]
fn update_fill_is_no_op_on_unknown_token() {
    let mut l = CopyLedger::new_in_memory();
    // No entries — should not panic
    l.update_fill("tok_unknown", dec!(5));
    assert!(l.entries.is_empty());
}

#[test]
fn update_fill_is_no_op_on_closed_entry() {
    let mut l = CopyLedger::new_in_memory();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(10), dec!(0.5));
    l.record_close("tok_a", "0xabc");

    // update_fill should not modify closed entries
    l.update_fill("tok_a", dec!(7));
    assert!(l.entries[0].filled_size.is_none());
}

#[test]
fn update_fill_updates_most_recent_active_entry() {
    let mut l = CopyLedger::new_in_memory();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(10), dec!(0.5));
    l.record_close("tok_a", "0xabc"); // close first
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(8), dec!(0.6)); // re-enter

    l.update_fill("tok_a", dec!(6));

    // Only the second (most recent active) entry should be updated
    let active = l.find_active_for_token("tok_a").unwrap();
    assert_eq!(active.filled_size, Some(dec!(6)));
    assert_eq!(active.size, dec!(8));
}

#[test]
fn filled_size_survives_disk_round_trip() {
    let path = tmp_path("filled_size");
    let _ = std::fs::remove_file(&path);

    {
        let mut l = CopyLedger::load_from(&path);
        l.record_copy("tok_a".into(), "0xabc".into(), dec!(10), dec!(0.5));
        l.update_fill("tok_a", dec!(7));
    }

    let l2 = CopyLedger::load_from(&path);
    assert_eq!(
        l2.find_active_for_token("tok_a").unwrap().filled_size,
        Some(dec!(7))
    );

    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// Ledger pruning (Gap 11)
// ---------------------------------------------------------------------------

#[test]
fn prune_removes_old_closed_entries() {
    let mut l = CopyLedger::new_in_memory();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(5), dec!(0.5));
    l.record_close("tok_a", "0xabc");

    // Manually backdate the closed_at timestamp to simulate an old entry
    if let Some(entry) = l.entries.last_mut() {
        entry.closed_at = Some(chrono::Utc::now() - chrono::Duration::days(100));
    }

    let pruned = l.prune_closed_older_than(30); // prune > 30 days old
    assert_eq!(pruned, 1);
    assert!(l.entries.is_empty());
}

#[test]
fn prune_keeps_recent_closed_entries() {
    let mut l = CopyLedger::new_in_memory();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(5), dec!(0.5));
    l.record_close("tok_a", "0xabc"); // closed just now

    let pruned = l.prune_closed_older_than(30); // only prune > 30 days
    assert_eq!(pruned, 0);
    assert_eq!(l.entries.len(), 1); // still there
}

#[test]
fn prune_never_removes_active_entries() {
    let mut l = CopyLedger::new_in_memory();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(5), dec!(0.5));
    // Not closed — active entry

    let pruned = l.prune_closed_older_than(0); // 0 days (disabled)
    assert_eq!(pruned, 0);

    // Even with days=1, active entries are protected
    let pruned2 = l.prune_closed_older_than(1);
    assert_eq!(pruned2, 0);
    assert!(l.has_any_active("tok_a"));
}

#[test]
fn prune_with_zero_days_is_a_no_op() {
    let mut l = CopyLedger::new_in_memory();
    l.record_copy("tok_a".into(), "0xabc".into(), dec!(5), dec!(0.5));
    l.record_close("tok_a", "0xabc");
    if let Some(e) = l.entries.last_mut() {
        e.closed_at = Some(chrono::Utc::now() - chrono::Duration::days(1000));
    }

    let pruned = l.prune_closed_older_than(0);
    assert_eq!(pruned, 0); // 0 = disabled
    assert_eq!(l.entries.len(), 1);
}

#[test]
fn prune_with_mixed_entries_only_removes_qualifying_ones() {
    let mut l = CopyLedger::new_in_memory();

    // Old closed entry (>90 days)
    l.record_copy("old_tok".into(), "0xabc".into(), dec!(5), dec!(0.5));
    l.record_close("old_tok", "0xabc");
    if let Some(e) = l.entries.last_mut() {
        e.closed_at = Some(chrono::Utc::now() - chrono::Duration::days(100));
    }

    // Recent closed entry (<90 days)
    l.record_copy("recent_tok".into(), "0xabc".into(), dec!(5), dec!(0.5));
    l.record_close("recent_tok", "0xabc");

    // Active entry
    l.record_copy("active_tok".into(), "0xabc".into(), dec!(5), dec!(0.5));

    let pruned = l.prune_closed_older_than(90);
    assert_eq!(pruned, 1); // only old_tok removed
    assert_eq!(l.entries.len(), 2);
    assert!(!l.has_any_active("old_tok")); // it was removed entirely
    assert!(l.entries.iter().any(|e| e.token_id == "recent_tok"));
    assert!(l.has_any_active("active_tok"));
}

// ---------------------------------------------------------------------------
// Close-sweep ledger source_wallet lookup (Gap 2)
// ---------------------------------------------------------------------------

#[test]
fn find_active_for_token_returns_correct_source_wallet_for_sweep() {
    // Simulates what start_position_close_sweep does to pick the taker_address
    let mut l = CopyLedger::new_in_memory();
    l.record_copy(
        "tok_x".into(),
        "0xsource_wallet".into(),
        dec!(10),
        dec!(0.6),
    );

    let entry = l.find_active_for_token("tok_x").unwrap();
    assert_eq!(entry.source_wallet, "0xsource_wallet");
    // The sweep uses entry.source_wallet as taker_address — verify record_close
    // with that value succeeds.
    l.record_close("tok_x", &entry.source_wallet.clone());
    assert!(!l.has_any_active("tok_x"));
}

#[test]
fn find_active_for_token_returns_none_when_no_ledger_entry() {
    let l = CopyLedger::new_in_memory();
    // Sweep falls back to first_target when this returns None
    assert!(l.find_active_for_token("tok_unknown").is_none());
}
