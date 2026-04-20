pub fn format_timestamp(ts: i64) -> String {
    // Basic formatting placeholder
    format!("{}", ts)
}

use alloy::primitives::Address;

use polymarket_client_sdk::data::types::response::Position;
use polymarket_client_sdk::data::Client as DataClient;

/// Fetches all active positions for a user by paginating through the Data API.
/// Resolves the bug where active wallets with >100 positions prematurely fall out
/// of the sweep logic and trigger false exits.
pub async fn fetch_all_positions(
    _client: &DataClient,
    user: Address,
) -> anyhow::Result<Vec<Position>> {
    let mut all_positions = Vec::new();
    let mut offset = 0;
    let limit = 500;
    // Hard cap guard to prevent runaway memory or looping
    let max_iterations = 100; // Raised to 100 for whale wallets with >10k open positions
    let mut iterations = 0;
    let reqwest_client = reqwest::Client::new();

    loop {
        iterations += 1;
        if iterations > max_iterations {
            tracing::warn!("fetch_all_positions forcibly breaking infinite loop threshold.");
            break;
        }

        let url = format!(
            "https://data-api.polymarket.com/positions?user={}&limit={}&offset={}&active=true",
            user, limit, offset
        );

        let fetch_fut = reqwest_client.get(&url).send();
        let timeout_fut = tokio::time::timeout(std::time::Duration::from_secs(15), fetch_fut).await;

        let res = match timeout_fut {
            Ok(Ok(response)) => response,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                let msg = format!("fetch_all_positions network timeout for {}", user);
                tracing::warn!("{}", msg);
                return Err(anyhow::anyhow!(msg));
            }
        };

        if !res.status().is_success() {
            let msg = format!(
                "fetch_all_positions returned HTTP {} for {}",
                res.status(),
                user
            );
            tracing::warn!("{}", msg);
            return Err(anyhow::anyhow!(msg));
        }

        let mut batch: Vec<Position> = match res.json().await {
            Ok(json) => json,
            Err(e) => return Err(e.into()),
        };

        let count = batch.len();
        all_positions.append(&mut batch);

        if count < (limit as usize) {
            break;
        }

        offset += limit;
    }

    Ok(all_positions)
}
