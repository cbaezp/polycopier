//! Tests for `src/backoff.rs` — Gap 6.
//! Run: `cargo test --test backoff_tests`

use polycopier::backoff::next_backoff;

#[test]
fn backoff_zero_errors_returns_base() {
    assert_eq!(next_backoff(0, 2, 120), 2);
}

#[test]
fn backoff_doubles_on_each_error() {
    assert_eq!(next_backoff(1, 2, 120), 4);
    assert_eq!(next_backoff(2, 2, 120), 8);
    assert_eq!(next_backoff(3, 2, 120), 16);
    assert_eq!(next_backoff(4, 2, 120), 32);
    assert_eq!(next_backoff(5, 2, 120), 64);
}

#[test]
fn backoff_caps_at_max() {
    assert_eq!(next_backoff(6, 2, 120), 120); // 2^6 * 2 = 128 → capped at 120
    assert_eq!(next_backoff(7, 2, 120), 120);
    assert_eq!(next_backoff(99, 2, 120), 120);
}

#[test]
fn backoff_cap_exponent_at_6_prevents_overflow() {
    // Without the cap, 2^63 * base would overflow u64.
    // With cap at 2^6 = 64, max delay = 64 * base.
    let big_base: u64 = 1_000_000_000;
    let result = next_backoff(100, big_base, u64::MAX);
    assert_eq!(result, big_base * 64); // 2^6 = 64
}

#[test]
fn backoff_with_base_30_and_max_300() {
    assert_eq!(next_backoff(0, 30, 300), 30);
    assert_eq!(next_backoff(1, 30, 300), 60);
    assert_eq!(next_backoff(2, 30, 300), 120);
    assert_eq!(next_backoff(3, 30, 300), 240);
    assert_eq!(next_backoff(4, 30, 300), 300); // 480 > 300 → capped
}

#[test]
fn backoff_with_base_10_and_max_60() {
    assert_eq!(next_backoff(0, 10, 60), 10);
    assert_eq!(next_backoff(1, 10, 60), 20);
    assert_eq!(next_backoff(2, 10, 60), 40);
    assert_eq!(next_backoff(3, 10, 60), 60); // 80 > 60 → capped
}
