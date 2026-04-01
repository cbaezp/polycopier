//! Tests for listener backpressure (Gap 10) and dedup ring-buffer.
//! Run: `cargo test --test listener_tests`
//!
//! Note: `start_ws_listener` itself requires a live Polymarket API connection,
//! so we test only the pure helpers that are extractable without the network.
//! The dedup eviction function is tested here via its observable behaviour
//! (re-insertion after eviction).

use std::collections::{HashSet, VecDeque};

// Replicate the pure evict_and_insert helper from listener.rs so we can test
// it directly without spawning an async task.
fn evict_and_insert(
    seen: &mut HashSet<String>,
    order: &mut VecDeque<String>,
    cap: usize,
    hash: String,
) {
    if order.len() >= cap {
        if let Some(oldest) = order.pop_front() {
            seen.remove(&oldest);
        }
    }
    seen.insert(hash.clone());
    order.push_back(hash);
}

// ---------------------------------------------------------------------------
// Dedup ring-buffer (SEEN_HASHES_CAP = 500 in production)
// ---------------------------------------------------------------------------

#[test]
fn dedup_ring_evicts_oldest_when_at_capacity() {
    let cap = 5usize;
    let mut seen: HashSet<String> = HashSet::new();
    let mut order: VecDeque<String> = VecDeque::new();

    // Fill to capacity
    for i in 0..cap {
        evict_and_insert(&mut seen, &mut order, cap, format!("hash_{}", i));
    }
    assert_eq!(seen.len(), cap);
    assert!(seen.contains("hash_0"));

    // One more — oldest (hash_0) should be evicted
    evict_and_insert(&mut seen, &mut order, cap, "hash_5".to_string());
    assert_eq!(seen.len(), cap);
    assert!(!seen.contains("hash_0"), "hash_0 should have been evicted");
    assert!(seen.contains("hash_5"));
    assert!(seen.contains("hash_1")); // next oldest — still present
}

#[test]
fn dedup_ring_does_not_evict_when_below_cap() {
    let cap = 500usize;
    let mut seen: HashSet<String> = HashSet::new();
    let mut order: VecDeque<String> = VecDeque::new();

    // Insert 10 hashes — well below cap
    for i in 0..10 {
        evict_and_insert(&mut seen, &mut order, cap, format!("hash_{}", i));
    }
    assert_eq!(seen.len(), 10);
    // All should still be present
    for i in 0..10 {
        assert!(seen.contains(&format!("hash_{}", i)));
    }
}

#[test]
fn dedup_ring_allows_reinsertion_after_eviction() {
    let cap = 3usize;
    let mut seen: HashSet<String> = HashSet::new();
    let mut order: VecDeque<String> = VecDeque::new();

    // Fill
    evict_and_insert(&mut seen, &mut order, cap, "a".to_string());
    evict_and_insert(&mut seen, &mut order, cap, "b".to_string());
    evict_and_insert(&mut seen, &mut order, cap, "c".to_string());

    // Evict "a" by inserting "d"
    evict_and_insert(&mut seen, &mut order, cap, "d".to_string());
    assert!(!seen.contains("a"));

    // "a" can now be re-inserted (e.g., if a hash is seen again after rotation)
    evict_and_insert(&mut seen, &mut order, cap, "a".to_string());
    assert!(seen.contains("a"));
}

#[test]
fn dedup_ring_does_not_double_count_duplicate_hashes() {
    let cap = 10usize;
    let mut seen: HashSet<String> = HashSet::new();
    let mut order: VecDeque<String> = VecDeque::new();

    // Insert the same hash twice — second is a new entry in `order` but `seen` deduplicates
    evict_and_insert(&mut seen, &mut order, cap, "dup".to_string());
    evict_and_insert(&mut seen, &mut order, cap, "dup".to_string());

    assert_eq!(seen.len(), 1); // only one unique hash
}

// ---------------------------------------------------------------------------
// Backpressure: try_send semantics (Gap 10)
// ---------------------------------------------------------------------------

#[test]
fn try_send_returns_full_when_channel_at_capacity() {
    // Use a bounded channel with capacity 1 to test backpressure without async
    let (tx, _rx) = tokio::sync::mpsc::channel::<u32>(1);

    // Fill the channel
    assert!(tx.try_send(1).is_ok());
    // Now full — try_send should return TrySendError::Full
    match tx.try_send(2) {
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {} // expected
        other => panic!("Expected Full, got: {:?}", other),
    }
}

#[test]
fn try_send_returns_closed_when_receiver_dropped() {
    let (tx, rx) = tokio::sync::mpsc::channel::<u32>(10);
    drop(rx); // close the receiver

    match tx.try_send(1) {
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {} // expected
        other => panic!("Expected Closed, got: {:?}", other),
    }
}
