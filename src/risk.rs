use crate::config::Config;
use crate::models::TradeEvent;
use rust_decimal::Decimal;

pub struct RiskEngine {
    config: Config,
    // Store daily PNL limits, consecutive losses, rapid flips limits here
}

impl RiskEngine {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub fn check_trade(&mut self, trade: &TradeEvent) -> Result<(), String> {
        let trade_value = trade.size * trade.price;

        // Prevent spoofing: ignore micro-trades below $1
        let min_value = Decimal::from(1);
        if trade_value < min_value {
            return Err("Trade value is too small (spoofing protection)".to_string());
        }

        // Cap: don't mirror trades larger than our configured max position size
        if trade_value > self.config.max_trade_size_usd {
            // We'll scale it down in strategy.rs — just warn, don't reject
        }

        Ok(())
    }
}
