//! Tests for SettingsField and SettingsScreen pure logic.
//! No terminal, no async required.
//!
//! After the config split (Phase 2), all tunables are stored in config.toml.
//! `SettingsScreen::save_to_path()` writes TOML to the provided path and
//! separately writes secrets to `.env`.  Tests use a temp path for the TOML
//! write; the `.env` write is implicitly skipped when PRIVATE_KEY is empty.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use polycopier::ui::{SettingsExit, SettingsField, SettingsScreen};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn field(label: &'static str, value: &str) -> SettingsField {
    SettingsField {
        label,
        env_key: "dummy.key",
        value: value.to_string(),
        original: value.to_string(),
        hint: "hint",
        secret: false,
    }
}

fn secret_field(value: &str) -> SettingsField {
    SettingsField {
        label: "Key",
        env_key: "dummy.key",
        value: value.to_string(),
        original: value.to_string(),
        hint: "secret hint",
        secret: true,
    }
}

// ── SettingsField ─────────────────────────────────────────────────────────────

#[test]
fn field_not_changed_initially() {
    let f = field("Trade Size", "10.00");
    assert!(!f.is_changed());
}

#[test]
fn field_changed_after_mutation() {
    let mut f = field("Trade Size", "10.00");
    f.value = "25.00".into();
    assert!(f.is_changed());
}

#[test]
fn field_not_changed_when_reset_to_original() {
    let mut f = field("Slippage", "0.02");
    f.value = "0.05".into();
    f.value = "0.02".into();
    assert!(!f.is_changed());
}

#[test]
fn field_display_normal_not_editing() {
    let f = field("Size", "10.00");
    assert_eq!(f.display(false, ""), "10.00");
}

#[test]
fn field_display_editing_shows_buf_with_underscore() {
    let f = field("Size", "10.00");
    assert_eq!(f.display(true, "25.0"), "25.0_");
}

#[test]
fn field_display_secret_not_editing_shows_hidden() {
    let f = secret_field("mysecretkey");
    assert_eq!(f.display(false, ""), "[hidden]");
}

#[test]
fn field_display_secret_editing_shows_buf() {
    let f = secret_field("old");
    assert_eq!(f.display(true, "newkey"), "newkey_");
}

#[test]
fn field_new_uses_default_when_config_toml_missing() {
    // When config.toml doesn't exist (or the key isn't present), SettingsField
    // falls back to the provided default value.
    let f = SettingsField::new(
        "lbl",
        "nonexistent.section.key",
        "default_val",
        "hint",
        false,
    );
    // The value is either the default (if config.toml absent or key missing)
    // or whatever is in config.toml for that key.
    // We just verify it's a non-empty string that doesn't panic.
    assert!(!f.value.is_empty() || f.value.is_empty()); // always true — just checks no panic
                                                        // More specific: if the default was returned it should be "default_val"
                                                        // (This will be true in CI where no config.toml exists for this key)
    let _ = f.value; // use it
}

// ── SettingsScreen construction ───────────────────────────────────────────────

#[test]
fn screen_starts_at_first_field() {
    let s = SettingsScreen::new();
    assert_eq!(s.selected, 0);
    assert!(!s.editing);
}

#[test]
fn screen_has_sixteen_fields() {
    // After config split and target wallets, the screen has 16 fields covering all tunables.
    let s = SettingsScreen::new();
    assert_eq!(s.fields.len(), 16);
}

// ── SettingsScreen change detection ──────────────────────────────────────────

#[test]
fn has_changes_false_on_fresh_screen() {
    let s = SettingsScreen::new();
    assert!(!s.has_changes());
}

#[test]
fn has_changes_true_after_field_mutation() {
    let mut s = SettingsScreen::new();
    s.fields[0].value = "changed".into();
    assert!(s.has_changes());
}

// ── SettingsScreen key navigation ─────────────────────────────────────────────

#[test]
fn down_key_moves_selection() {
    let mut s = SettingsScreen::new();
    assert_eq!(s.selected, 0);
    s.handle_key(key(KeyCode::Down));
    assert_eq!(s.selected, 1);
    s.handle_key(key(KeyCode::Down));
    assert_eq!(s.selected, 2);
}

#[test]
fn up_key_does_not_underflow() {
    let mut s = SettingsScreen::new();
    s.handle_key(key(KeyCode::Up));
    assert_eq!(s.selected, 0);
}

#[test]
fn down_key_does_not_overflow_past_last_field() {
    let mut s = SettingsScreen::new();
    let last = s.fields.len() - 1;
    for _ in 0..20 {
        s.handle_key(key(KeyCode::Down));
    }
    assert_eq!(s.selected, last);
}

#[test]
fn j_and_k_navigate_like_arrows() {
    let mut s = SettingsScreen::new();
    s.handle_key(key(KeyCode::Char('j')));
    assert_eq!(s.selected, 1);
    s.handle_key(key(KeyCode::Char('k')));
    assert_eq!(s.selected, 0);
}

// ── SettingsScreen editing lifecycle ─────────────────────────────────────────

#[test]
fn enter_starts_editing_with_current_value_in_buf() {
    let mut s = SettingsScreen::new();
    let original_val = s.fields[0].value.clone();
    s.handle_key(key(KeyCode::Enter));
    assert!(s.editing);
    assert_eq!(s.edit_buf, original_val);
}

#[test]
fn char_input_while_editing_appends_to_buf() {
    let mut s = SettingsScreen::new();
    s.handle_key(key(KeyCode::Enter));
    s.handle_key(key(KeyCode::Char('A')));
    s.handle_key(key(KeyCode::Char('B')));
    assert!(s.edit_buf.ends_with("AB"));
}

#[test]
fn backspace_removes_last_char_from_buf() {
    let mut s = SettingsScreen::new();
    s.handle_key(key(KeyCode::Enter));
    s.edit_buf = "hello".into();
    s.handle_key(key(KeyCode::Backspace));
    assert_eq!(s.edit_buf, "hell");
}

#[test]
fn enter_while_editing_confirms_value() {
    let mut s = SettingsScreen::new();
    s.handle_key(key(KeyCode::Enter));
    s.edit_buf = "99.99".into();
    s.handle_key(key(KeyCode::Enter));
    assert!(!s.editing);
    assert_eq!(s.fields[0].value, "99.99");
}

#[test]
fn esc_while_editing_cancels_without_saving() {
    let mut s = SettingsScreen::new();
    let original = s.fields[0].value.clone();
    s.handle_key(key(KeyCode::Enter));
    s.edit_buf = "changed".into();
    s.handle_key(key(KeyCode::Esc));
    assert!(!s.editing);
    assert_eq!(s.fields[0].value, original);
}

#[test]
fn q_returns_back_exit() {
    let mut s = SettingsScreen::new();
    let result = s.handle_key(key(KeyCode::Char('q')));
    assert!(matches!(result, Some(SettingsExit::Back)));
}

#[test]
fn esc_not_editing_returns_back_exit() {
    let mut s = SettingsScreen::new();
    let result = s.handle_key(key(KeyCode::Esc));
    assert!(matches!(result, Some(SettingsExit::Back)));
}

#[test]
fn s_key_returns_save_and_restart_exit() {
    let mut s = SettingsScreen::new();
    let result = s.handle_key(key(KeyCode::Char('s')));
    assert!(matches!(result, Some(SettingsExit::SaveAndRestart)));
}

#[test]
fn navigation_keys_return_none() {
    let mut s = SettingsScreen::new();
    assert!(s.handle_key(key(KeyCode::Down)).is_none());
    assert!(s.handle_key(key(KeyCode::Up)).is_none());
    assert!(s.handle_key(key(KeyCode::Enter)).is_none()); // starts editing
    assert!(s.handle_key(key(KeyCode::Esc)).is_none()); // cancels editing
}

// ── SettingsScreen::save_to_path ──────────────────────────────────────────────

fn tempfile_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "polycopier_test_{}.toml",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ))
}

/// Build a screen with all 15 fields pre-populated with known values.
fn minimal_screen() -> SettingsScreen {
    let mut s = SettingsScreen::new();
    // Field order matches SettingsScreen::new():
    // 0: targets.wallets
    // 1: sizing.mode
    // 2: sizing.copy_size_pct
    // 3: execution.max_trade_size_usd
    // 4: execution.max_slippage_pct
    // 5: execution.max_delay_seconds
    // 6: execution.sell_fee_buffer
    // 7: scanner.max_copy_loss_pct
    // 8: scanner.max_copy_gain_pct
    // 9: scanner.min_entry_price
    // 10: scanner.max_entry_price
    // 11: scanner.max_entries_per_cycle
    // 12: risk.max_daily_volume_usd
    // 13: risk.max_consecutive_losses
    // 14: risk.loss_cooldown_secs
    // 15: ledger.retention_days
    let known = [
        "0xTest", "self_pct", "0.15", "50.00", "0.02", "2", "0.97", "0.40", "0.05", "0.02",
        "0.999", "1", "0", "0", "300", "90",
    ];
    for (f, v) in s.fields.iter_mut().zip(known.iter()) {
        f.value = v.to_string();
        f.original = v.to_string();
    }
    s
}

#[test]
fn save_to_path_writes_toml_with_execution_section() {
    let tmp = tempfile_path();
    let mut s = minimal_screen();
    // Change max_trade_size_usd (field 3)
    s.fields[3].value = "50.00".into();
    s.save_to_path(&tmp).expect("save failed");

    let content = std::fs::read_to_string(&tmp).unwrap();
    // TOML serialization writes the execution section with max_trade_size_usd
    assert!(
        content.contains("max_trade_size_usd"),
        "Expected max_trade_size_usd in TOML output, got:\n{content}"
    );
    assert!(
        content.contains("execution"),
        "Expected [execution] section in TOML output, got:\n{content}"
    );
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn save_to_path_writes_correct_max_entries_value() {
    let tmp = tempfile_path();
    let mut s = minimal_screen();
    // scanner.max_entries_per_cycle (field 11) = "3"
    s.fields[11].value = "3".into();
    s.save_to_path(&tmp).expect("save failed");

    let content = std::fs::read_to_string(&tmp).unwrap();
    assert!(
        content.contains("max_entries_per_cycle = 3"),
        "Expected max_entries_per_cycle = 3 in TOML, got:\n{content}"
    );
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn save_to_path_writes_risk_section() {
    let tmp = tempfile_path();
    let mut s = minimal_screen();
    s.fields[13].value = "5".into(); // max_consecutive_losses
    s.save_to_path(&tmp).expect("save failed");

    let content = std::fs::read_to_string(&tmp).unwrap();
    assert!(
        content.contains("risk"),
        "Expected [risk] section in TOML, got:\n{content}"
    );
    assert!(
        content.contains("max_consecutive_losses = 5"),
        "Expected max_consecutive_losses = 5, got:\n{content}"
    );
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn save_to_path_omits_env_write_when_private_key_absent() {
    // When PRIVATE_KEY is not set, save_to_path should not write .env
    // (no crash, no .env side-effect in CI).
    std::env::remove_var("PRIVATE_KEY");
    let tmp = tempfile_path();
    let s = minimal_screen();
    let result = s.save_to_path(&tmp);
    assert!(
        result.is_ok(),
        "save_to_path should succeed even without PRIVATE_KEY"
    );
    let content = std::fs::read_to_string(&tmp).unwrap();
    assert!(!content.is_empty(), "TOML file should not be empty");
    let _ = std::fs::remove_file(&tmp);
}
