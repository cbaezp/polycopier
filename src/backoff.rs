//! Exponential backoff helper used by all background polling tasks.
//!
//! This is a pure stateless calculation — callers own the `consecutive_errors`
//! counter and reset it to 0 on success.

/// Compute the next sleep duration (seconds) after `consecutive_errors` failures.
///
/// Formula: `min(base_secs * 2^consecutive_errors, max_secs)`
///
/// The exponent is capped at 6 to prevent integer overflow on very long outages
/// (2^6 = 64, so the cap kicks in well before overflow).
///
/// # Examples
/// ```
/// use polycopier::backoff::next_backoff;
/// assert_eq!(next_backoff(0, 2, 120), 2);   // first failure — base delay
/// assert_eq!(next_backoff(1, 2, 120), 4);   // second failure — 2x
/// assert_eq!(next_backoff(6, 2, 120), 120); // saturates at max_secs
/// assert_eq!(next_backoff(99, 2, 120), 120);// still saturates
/// ```
pub fn next_backoff(consecutive_errors: u32, base_secs: u64, max_secs: u64) -> u64 {
    let exp = consecutive_errors.min(6);
    let delay = base_secs.saturating_mul(1u64 << exp);
    delay.min(max_secs)
}
