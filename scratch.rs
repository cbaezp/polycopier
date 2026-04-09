use dotenvy::dotenv;
use std::env;
use rust_decimal::Decimal;
use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct ApiPosition {
    pub asset: String,
    pub size: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    let addr = env::var("FUNDER_ADDRESS").unwrap_or_default();
    println!("Checking holdings for Funder: {}", addr);
    
    let url = format!("https://gamma-api.polymarket.com/positions?user={}", addr);
    let resp: Vec<ApiPosition> = reqwest::get(&url).await?.json().await?;
    
    let tokens = vec![
        "114040165731", "758471519535", "263949933844",
        "988870812060", "787327820270", "500727360138",
        "846430688566", "114176459472"
    ];
    
    for t in tokens {
        let pos = resp.iter().find(|p| p.asset.starts_with(t));
        println!("Token {}: {:?}", t, pos);
    }
    
    Ok(())
}
