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
