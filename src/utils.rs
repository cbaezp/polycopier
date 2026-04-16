pub fn format_timestamp(ts: i64) -> String {
    // Basic formatting placeholder
    format!("{}", ts)
}

use alloy::primitives::Address;
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_client_sdk::data::types::response::Position;
use polymarket_client_sdk::data::Client as DataClient;

/// Fetches all active positions for a user by paginating through the Data API.
/// Resolves the bug where active wallets with >100 positions prematurely fall out
/// of the sweep logic and trigger false exits.
pub async fn fetch_all_positions(
    client: &DataClient,
    user: Address,
) -> anyhow::Result<Vec<Position>> {
    let mut all_positions = Vec::new();
    let mut offset = 0;
    let limit = 500;
    // Hard cap guard to prevent runaway memory or looping
    let max_iterations = 20;
    let mut iterations = 0;

    loop {
        iterations += 1;
        if iterations > max_iterations {
            tracing::warn!("fetch_all_positions forcibly breaking infinite loop threshold.");
            break;
        }

        let req = PositionsRequest::builder()
            .user(user)
            .limit(limit)
            .unwrap()
            .offset(offset)
            .unwrap()
            .build();

        // Enforce a strict 15-second timeout on the network call itself
        // so we don't permanently hang the background loop if the API stalls.
        let fetch_fut = client.positions(&req);
        let batch_result =
            tokio::time::timeout(std::time::Duration::from_secs(15), fetch_fut).await;

        let mut batch = match batch_result {
            Ok(Ok(data)) => data,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                // Timeout occurred
                tracing::warn!("fetch_all_positions network timeout; breaking loop early.");
                break;
            }
        };

        let count = batch.len();
        all_positions.append(&mut batch);

        if count < (limit as usize) {
            break;
        }

        offset += limit;
        if offset >= 10000 {
            break;
        }
    }

    Ok(all_positions)
}
