//! Persistent copy ledger — tracks every position this bot has entered, including
//! which target wallet the copy originated from.
//!
//! ## Design
//!
//! The ledger is the authoritative record of **intent**: it answers "did we copy token X
//! from target T?", which the live Polymarket API alone cannot answer after the fact because
//! a position that has been closed no longer appears in the `/positions` endpoint.
//!
//! ## One-position-per-token rule
//!
//! The ledger enforces that we only ever hold one position per token ID.  If two targets
//! both enter the same token, we copy from the **first** one and skip subsequent entries.
//! This prevents double-sizing and makes SELL intent unambiguous: only the target we
//! originally copied from can trigger a close.
//!
//! ## Persistence (atomic — Gap 1)
//!
//! Written atomically: content is first written to `copy_ledger.json.tmp`, then
//! `rename()`d over `copy_ledger.json`.  On POSIX systems rename() is atomic, so a
//! crash mid-write can never leave a corrupt or truncated ledger file.
//!
//! ## Partial-fill tracking (Gap 4)
//!
//! `CopyEntry.filled_size` records the actual number of shares filled (updated by the
//! 30s position-sync task).  The `size` field retains the original order size so both
//! values are available for diagnostics.
//!
//! ## Pruning (Gap 11)
//!
//! `prune_closed_older_than(days)` removes closed entries whose `closed_at` timestamp
//! is older than `days` UTC days.  Call this at startup after `reconcile`.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const LEDGER_PATH: &str = "copy_ledger.json";

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// A single copy-trade record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyEntry {
    /// Polymarket outcome token ID (U256 as decimal string).
    pub token_id: String,
    /// Target wallet we copied this position from (normalized to lowercase).
    pub source_wallet: String,
    /// Number of shares in the SUBMITTED order (may exceed actual fill).
    pub size: Decimal,
    /// Actual shares filled, updated by the position-sync task.
    /// `None` until the first 30s sync that confirms a fill.
    pub filled_size: Option<Decimal>,
    /// Limit price used when submitting the entry order.
    pub entry_price: Decimal,
    /// When we submitted the entry order.
    pub copied_at: DateTime<Utc>,
    /// Whether this position has been closed.
    pub closed: bool,
    /// When the position was closed (if applicable).
    pub closed_at: Option<DateTime<Utc>>,
}

/// In-memory copy of the on-disk ledger.
#[derive(Debug, Serialize, Deserialize)]
pub struct CopyLedger {
    pub entries: Vec<CopyEntry>,
    /// Path to the backing JSON file.  Empty string means in-memory only
    /// (no disk persistence) — used in unit tests.
    #[serde(skip)]
    path: String,
}

impl Default for CopyLedger {
    /// Returns an **in-memory** ledger with no disk backing.
    /// Use [`CopyLedger::load`] or [`CopyLedger::load_from`] to get a
    /// persisted ledger.
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            path: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Core operations
// ---------------------------------------------------------------------------

impl CopyLedger {
    // -- Constructors -------------------------------------------------------

    /// Load from `copy_ledger.json`, or start an empty ledger if the file
    /// does not exist or cannot be parsed.
    pub fn load() -> Self {
        Self::load_from(LEDGER_PATH)
    }

    /// Load from an arbitrary path.  Useful for testing with temp files.
    pub fn load_from(path: &str) -> Self {
        let mut ledger = match std::fs::read_to_string(path) {
            Ok(content) => serde_json::from_str::<Self>(&content).unwrap_or_else(|e| {
                tracing::warn!("copy_ledger parse error at {path} — starting fresh: {e}");
                Self::default()
            }),
            Err(_) => {
                tracing::info!("copy_ledger not found at {path} — starting a new ledger.");
                let l = Self {
                    path: path.to_string(),
                    ..Default::default()
                };

                l.save(); // Force write at startup so user knows it exists
                return l;
            }
        };
        ledger.path = path.to_string();
        ledger
    }

    /// Create an empty, in-memory-only ledger with no disk persistence.
    /// Suitable for unit tests.
    pub fn new_in_memory() -> Self {
        Self::default()
    }

    // -- Persistence (atomic) -- Gap 1 -------------------------------------

    /// Write the ledger to disk atomically via a `.tmp` sibling + rename.
    /// No-op if this is an in-memory-only ledger (path is empty).
    /// Logs a warning on failure but never panics.
    pub fn save(&self) {
        if self.path.is_empty() {
            return; // in-memory mode — no disk persistence
        }
        let tmp = format!("{}.tmp", self.path);
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&tmp, &json) {
                    tracing::warn!("Failed to write tmp ledger {}: {e}", tmp);
                    return;
                }
                if let Err(e) = std::fs::rename(&tmp, &self.path) {
                    tracing::warn!("Failed to rename tmp ledger to {}: {e}", self.path);
                }
            }
            Err(e) => tracing::warn!("Failed to serialise copy_ledger: {e}"),
        }
    }

    // -- Mutations ----------------------------------------------------------

    /// Record that we entered a new position.
    ///
    /// Panics in debug builds if an active copy for `token_id` already exists
    /// (the caller should have checked [`has_any_active`] first).
    pub fn record_copy(
        &mut self,
        token_id: String,
        source_wallet: String,
        size: Decimal,
        entry_price: Decimal,
    ) {
        debug_assert!(
            !self.has_any_active(&token_id),
            "record_copy called but an active copy for {token_id} already exists"
        );
        self.entries.push(CopyEntry {
            token_id,
            source_wallet,
            size,
            filled_size: None,
            entry_price,
            copied_at: Utc::now(),
            closed: false,
            closed_at: None,
        });
        self.save();
    }

    /// Mark the most recent **active** entry for `(token_id, source_wallet)` as closed.
    pub fn record_close(&mut self, token_id: &str, source_wallet: &str) {
        let now = Utc::now();
        for entry in self.entries.iter_mut().rev() {
            if !entry.closed && entry.token_id == token_id && entry.source_wallet == source_wallet {
                entry.closed = true;
                entry.closed_at = Some(now);
                self.save();
                return;
            }
        }
        tracing::warn!(
            "record_close: no active entry found for token {} from {}",
            &token_id[..token_id.len().min(12)],
            &source_wallet[..source_wallet.len().min(10)],
        );
    }

    /// Update the filled size for the most recent active entry for `token_id`. (Gap 4)
    ///
    /// Called by the position-sync task when it observes a position size that
    /// differs from the recorded order size.
    pub fn update_fill(&mut self, token_id: &str, actual_size: Decimal) {
        for entry in self.entries.iter_mut().rev() {
            if !entry.closed && entry.token_id == token_id {
                entry.filled_size = Some(actual_size);
                self.save();
                return;
            }
        }
    }

    // -- Queries ------------------------------------------------------------

    /// Whether we have **any** active (unclosed) copy for `token_id`, regardless of
    /// which target wallet it came from.
    ///
    /// Used by the BUY gate to enforce the one-position-per-token rule.
    pub fn has_any_active(&self, token_id: &str) -> bool {
        self.find_active_for_token(token_id).is_some()
    }

    /// Return the active entry for `token_id`, if one exists.
    ///
    /// Because of the one-position-per-token rule there can be at most one.
    pub fn find_active_for_token(&self, token_id: &str) -> Option<&CopyEntry> {
        self.entries
            .iter()
            .rev()
            .find(|e| !e.closed && e.token_id == token_id)
    }

    /// Whether we have an active copy for `token_id` specifically from `source_wallet`.
    pub fn has_active_copy(&self, token_id: &str, source_wallet: &str) -> bool {
        self.entries
            .iter()
            .rev()
            .any(|e| !e.closed && e.token_id == token_id && e.source_wallet == source_wallet)
    }

    // -- Startup reconciliation --------------------------------------------

    /// Close any ledger entries for tokens we no longer hold in our live wallet.
    ///
    /// Call this once at startup, after seeding `state.positions` from the Polymarket
    /// API.  It corrects for positions that were closed while the bot was offline.
    pub fn reconcile(&mut self, live_our_token_ids: &HashSet<String>) {
        let now = Utc::now();
        let mut changed = false;

        for entry in self.entries.iter_mut() {
            if entry.closed {
                continue;
            }
            if !live_our_token_ids.contains(&entry.token_id) {
                tracing::warn!(
                    "Ledger reconcile: token {} (from {}) no longer in wallet — marking closed.",
                    &entry.token_id[..entry.token_id.len().min(12)],
                    &entry.source_wallet[..entry.source_wallet.len().min(10)],
                );
                entry.closed = true;
                entry.closed_at = Some(now);
                changed = true;
            }
        }
        if changed {
            self.save();
        } else {
            tracing::info!("Ledger reconcile: all active entries match live wallet. No changes.");
        }
    }

    // -- Pruning (Gap 11) --------------------------------------------------

    /// Remove closed entries whose `closed_at` timestamp is older than `days` UTC days.
    ///
    /// Active (unclosed) entries are never removed.
    /// Returns the number of entries pruned.
    pub fn prune_closed_older_than(&mut self, days: u32) -> usize {
        if days == 0 {
            return 0; // 0 = disabled
        }
        let cutoff = Utc::now() - chrono::Duration::days(i64::from(days));
        let before = self.entries.len();
        self.entries.retain(|e| {
            if !e.closed {
                return true; // never prune active entries
            }
            // Keep if closed_at is recent or unknown
            e.closed_at.is_none_or(|t| t >= cutoff)
        });
        let pruned = before - self.entries.len();
        if pruned > 0 {
            tracing::info!(
                "Ledger: pruned {} closed entries older than {} days.",
                pruned,
                days
            );
            self.save();
        }
        pruned
    }
}
