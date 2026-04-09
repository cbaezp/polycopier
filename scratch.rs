use dotenvy::dotenv;
use std::env;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    // read config
    let config_raw = std::fs::read_to_string("config.toml")?;
    println!("Config:\n{}", config_raw);
    Ok(())
}
