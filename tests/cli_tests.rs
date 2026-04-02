use polycopier::config::parse_cli_args;

#[test]
fn test_cli_default_mode() {
    let args = vec!["polycopier".to_string()];
    let cli = parse_cli_args(&args);

    assert!(!cli.is_daemon, "Default mode should not be daemon");
    assert!(!cli.is_ui, "Default mode should not be UI");
    assert!(!cli.headless, "Default mode should not be headless");
    assert!(!cli.skip_open, "Default mode should not skip open (N/A)");
}

#[test]
fn test_cli_ui_mode() {
    let args = vec!["polycopier".to_string(), "--ui".to_string()];
    let cli = parse_cli_args(&args);

    assert!(!cli.is_daemon, "UI mode is not daemon");
    assert!(cli.is_ui, "UI mode should trigger is_ui");
    assert!(cli.headless, "UI mode implies headless for TUI elements");
    assert!(!cli.skip_open, "Base UI mode should trigger browser open");
}

#[test]
fn test_cli_ui_reboot_mode() {
    let args = vec!["polycopier".to_string(), "--ui-reboot".to_string()];
    let cli = parse_cli_args(&args);

    assert!(!cli.is_daemon, "Reboot mode is not daemon");
    assert!(cli.is_ui, "Reboot mode should trigger is_ui");
    assert!(
        cli.headless,
        "Reboot mode implies headless for TUI elements"
    );
    assert!(
        cli.skip_open,
        "Reboot mode must STRICTLY skip browser open to prevent duplicate tabs"
    );
}

#[test]
fn test_cli_daemon_mode() {
    let args = vec!["polycopier".to_string(), "--daemon".to_string()];
    let cli = parse_cli_args(&args);

    assert!(cli.is_daemon, "Daemon mode should trigger is_daemon");
    assert!(!cli.is_ui, "Daemon mode should NOT trigger is_ui");
    assert!(cli.headless, "Daemon mode implies headless");
}

#[test]
fn test_cli_headless_alias() {
    let args = vec!["polycopier".to_string(), "--headless".to_string()];
    let cli = parse_cli_args(&args);

    assert!(cli.is_daemon, "--headless is an alias for daemon");
    assert!(cli.headless);
}
