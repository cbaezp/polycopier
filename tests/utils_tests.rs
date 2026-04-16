use alloy::primitives::Address;
use polymarket_client_sdk::data::Client as DataClient;
use std::str::FromStr;

use polycopier::utils::fetch_all_positions;

#[tokio::test]
#[ignore = "Hits Live PolyMarket API"]
async fn test_fetch_all_positions_live_pagination() {
    // 0x7ea... is known to consistently have > 500 positions which proves
    // the pagination chunking successfully circumvents the 100-limit cap.
    let client = DataClient::default();
    let addr = Address::from_str("0x7ea571c40408f340c1c8fc8eaacebab53c1bde7b").unwrap();

    let positions = fetch_all_positions(&client, addr).await.unwrap();

    // We expect properly resolved offset accumulation > 100
    assert!(
        positions.len() > 100,
        "Should have bypassed the 100 limit, instead got: {}",
        positions.len()
    );

    println!(
        "Successfully paginated and found {} positions for target",
        positions.len()
    );
}
