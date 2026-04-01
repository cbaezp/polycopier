//! Tests for SettingsField and SettingsScreen pure logic.
//! No terminal, no async required.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use polycopier::ui::{SettingsExit, SettingsField, SettingsScreen};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn field(label: &'static str, value: &str) -> SettingsField {
    SettingsField {
        label,
        env_key: "DUMMY",
        value: value.to_string(),
        original: value.to_string(),
        hint: "hint",
        secret: false,
    }
}

fn secret_field(value: &str) -> SettingsField {
    SettingsField {
        label: "Key",
        env_key: "DUMMY",
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
fn field_new_strips_surrounding_quotes() {
    std::env::set_var("_TEST_QUOTED", "\"hello\"");
    let f = SettingsField::new("lbl", "_TEST_QUOTED", "", "hint", false);
    assert_eq!(f.value, "hello");
    std::env::remove_var("_TEST_QUOTED");
}

#[test]
fn field_new_uses_default_when_env_missing() {
    std::env::remove_var("_TEST_MISSING_FIELD");
    let f = SettingsField::new("lbl", "_TEST_MISSING_FIELD", "default_val", "hint", false);
    assert_eq!(f.value, "default_val");
}

// ── SettingsScreen construction ───────────────────────────────────────────────

#[test]
fn screen_starts_at_first_field() {
    let s = SettingsScreen::new();
    assert_eq!(s.selected, 0);
    assert!(!s.editing);
}

#[test]
fn screen_has_nine_fields() {
    let s = SettingsScreen::new();
    assert_eq!(s.fields.len(), 9);
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
        "polycopier_test_{}.env",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ))
}

fn minimal_screen() -> SettingsScreen {
    let mut s = SettingsScreen::new();
    let known = [
        "0xWallet", "10.00", "0.02", "2", "0.40", "0.02", "0.999", "self_pct", "",
    ];
    for (f, v) in s.fields.iter_mut().zip(known.iter()) {
        f.value = v.to_string();
        f.original = v.to_string();
    }
    s
}

#[test]
fn save_to_path_writes_non_empty_fields() {
    let tmp = tempfile_path();
    let mut s = minimal_screen();
    s.fields[0].value = "0xABC,0xDEF".into();
    s.fields[1].value = "50.00".into();
    s.save_to_path(&tmp).expect("save failed");

    let content = std::fs::read_to_string(&tmp).unwrap();
    assert!(content.contains("TARGET_WALLETS=\"0xABC,0xDEF\""));
    assert!(content.contains("MAX_TRADE_SIZE_USD=\"50.00\""));
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn save_to_path_skips_empty_fields() {
    let tmp = tempfile_path();
    let mut s = minimal_screen();
    s.fields[8].value = "".into(); // COPY_SIZE_PCT -- clear it
    s.save_to_path(&tmp).expect("save failed");

    let content = std::fs::read_to_string(&tmp).unwrap();
    assert!(
        !content.contains("COPY_SIZE_PCT"),
        "empty field should not be written"
    );
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn save_to_path_preserves_private_key_from_env() {
    std::env::set_var("PRIVATE_KEY", "\"0xdeadbeef\"");
    let tmp = tempfile_path();
    let s = minimal_screen();
    s.save_to_path(&tmp).expect("save failed");

    let content = std::fs::read_to_string(&tmp).unwrap();
    assert!(
        content.contains("PRIVATE_KEY=\"0xdeadbeef\""),
        "PRIVATE_KEY should be preserved from env, got:\n{}",
        content
    );
    let _ = std::fs::remove_file(&tmp);
    std::env::remove_var("PRIVATE_KEY");
}
