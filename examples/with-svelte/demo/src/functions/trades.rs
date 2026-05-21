use forge::prelude::*;
use futures_util::StreamExt;
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Trade record from Binance stream
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct Trade {
    pub id: Uuid,
    pub symbol: String,
    pub price: f64,
    pub quantity: f64,
    pub trade_time: Timestamp,
    pub is_buyer_maker: bool,
    pub created_at: Timestamp,
}

#[derive(Debug, serde::Deserialize)]
struct BinanceTrade {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "p")]
    price: String,
    #[serde(rename = "q")]
    quantity: String,
    #[serde(rename = "T")]
    trade_time: i64,
    #[serde(rename = "m")]
    is_buyer_maker: bool,
}

#[derive(Debug, PartialEq)]
struct ParsedTradeRecord {
    symbol: String,
    price: f64,
    quantity: f64,
    trade_time: Timestamp,
    is_buyer_maker: bool,
}

fn parse_trade_message(text: &str, fallback_time: Timestamp) -> Option<ParsedTradeRecord> {
    let trade = serde_json::from_str::<BinanceTrade>(text).ok()?;

    Some(ParsedTradeRecord {
        symbol: trade.symbol,
        price: trade.price.parse().unwrap_or(0.0),
        quantity: trade.quantity.parse().unwrap_or(0.0),
        trade_time: chrono::DateTime::from_timestamp_millis(trade.trade_time)
            .unwrap_or(fallback_time),
        is_buyer_maker: trade.is_buyer_maker,
    })
}

/// Get the 4 most recent trades
#[forge::query(auth = "none")]
pub async fn get_trades(ctx: &QueryContext) -> Result<Vec<Trade>> {
    sqlx::query_as!(
        Trade,
        r#"
        SELECT id, symbol, price, quantity, trade_time, is_buyer_maker, created_at
        FROM trades
        ORDER BY created_at DESC
        LIMIT 4
        "#
    )
    .fetch_all(ctx.db())
    .await
    .map_err(Into::into)
}

/// Streams live trades from Binance WebSocket and writes to database
#[forge::daemon(restart_on_panic = true, restart_delay = "5s")]
pub async fn trade_stream(ctx: &DaemonContext) -> Result<()> {
    if std::env::var_os("CI").is_some() {
        tracing::info!("Skipping Binance WebSocket in CI");
        ctx.shutdown_signal().await;
        return Ok(());
    }

    let url = "wss://stream.binance.com:9443/ws/eurusdt@trade";
    tracing::info!("Connecting to Binance WebSocket: {}", url);

    let (ws_stream, _) = connect_async(url)
        .await
        .map_err(|e| ForgeError::internal(format!("WebSocket connect failed: {}", e)))?;

    let (_, mut read) = ws_stream.split();
    tracing::info!("Connected to Binance trade stream");

    loop {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(trade) = parse_trade_message(&text, Utc::now()) {
                            sqlx::query!(
                                "INSERT INTO trades (id, symbol, price, quantity, trade_time, is_buyer_maker, created_at) \
                                 VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, NOW())",
                                &trade.symbol,
                                trade.price,
                                trade.quantity,
                                trade.trade_time,
                                trade.is_buyer_maker
                            )
                            .execute(ctx.db())
                            .await
                            .ok();
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        tracing::warn!("WebSocket closed by server");
                        break;
                    }
                    Some(Err(e)) => {
                        tracing::error!("WebSocket error: {}", e);
                        break;
                    }
                    None => break,
                    _ => {}
                }
            }
            _ = ctx.shutdown_signal() => {
                tracing::info!("Trade stream shutting down");
                break;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_parse_trade_message_extracts_trade_fields() {
        let fallback = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let parsed = parse_trade_message(
            r#"{"s":"EURUSDT","p":"1.1234","q":"250.50","T":1704067200000,"m":true}"#,
            fallback,
        )
        .unwrap();

        assert_eq!(parsed.symbol, "EURUSDT");
        assert_eq!(parsed.price, 1.1234);
        assert_eq!(parsed.quantity, 250.50);
        assert_eq!(parsed.trade_time, fallback);
        assert!(parsed.is_buyer_maker);
    }

    #[test]
    fn test_parse_trade_message_returns_none_for_invalid_json() {
        let fallback = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        assert!(parse_trade_message("not-json", fallback).is_none());
    }

    #[test]
    fn test_parse_trade_message_falls_back_for_invalid_numbers_and_timestamp() {
        let fallback = Utc.with_ymd_and_hms(2024, 5, 12, 8, 30, 0).unwrap();
        let parsed = parse_trade_message(
            r#"{"s":"EURUSDT","p":"oops","q":"nope","T":9223372036854775807,"m":false}"#,
            fallback,
        )
        .unwrap();

        assert_eq!(parsed.price, 0.0);
        assert_eq!(parsed.quantity, 0.0);
        assert_eq!(parsed.trade_time, fallback);
        assert!(!parsed.is_buyer_maker);
    }
}
