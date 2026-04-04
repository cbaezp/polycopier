use polycopier::state::BotState;
use rust_decimal::Decimal;

#[test]
fn test_bot_state_initializes_with_zero_for_live() {
    let state = BotState::new(false, None);
    assert_eq!(state.total_balance, Decimal::from(0));
}

#[test]
fn test_bot_state_initializes_with_ten_thousand_for_sim() {
    let state = BotState::new(true, None);
    assert_eq!(state.total_balance, Decimal::from(10000));
}

#[test]
fn test_bot_state_initializes_with_custom_sim_balance() {
    let state = BotState::new(true, Some(Decimal::from(500)));
    assert_eq!(state.total_balance, Decimal::from(500));
}
