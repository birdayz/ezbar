//! Stock quote. Port of pkg/datasource/stock.go.
//! Tries Yahoo Finance (no key), then Finnhub, then Alpha Vantage (need a key).

use std::time::Duration as StdDuration;

use serde_json::Value;

#[allow(dead_code)] // full quote fields mirror the Go model
#[derive(Debug, Clone, Default)]
pub struct StockData {
    pub symbol: String,
    pub price: f64,
    pub change: f64,
    pub change_percent: f64,
    pub display_text: String,
    pub price_string: String,
    pub change_string: String,
    pub is_positive: bool,
    pub is_negative: bool,
    pub trend_emoji: String,
}

pub fn config() -> (String, String) {
    let symbol = std::env::var("EZBAR_STOCK_SYMBOL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "NQ=F".to_string())
        .to_uppercase();
    let api_key = std::env::var("EZBAR_STOCK_API_KEY").unwrap_or_default();
    (symbol, api_key)
}

fn format_stock(symbol: &str, price: f64, change: f64, change_percent: f64) -> StockData {
    let is_positive = change >= 0.0;
    let (trend_emoji, sign) = if is_positive { ("📈", "+") } else { ("📉", "") };
    let price_string = format!("${:.2}", price);
    let change_string = format!("{}{:.2} ({:.2}%)", sign, change, change_percent);
    let display_text = format!("{} {}: {} {}", trend_emoji, symbol, price_string, change_string);
    StockData {
        symbol: symbol.to_string(),
        price,
        change,
        change_percent,
        display_text,
        price_string,
        change_string,
        is_positive,
        is_negative: change < 0.0,
        trend_emoji: trend_emoji.to_string(),
    }
}

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(StdDuration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())
}

pub async fn fetch(symbol: &str, api_key: &str) -> Result<StockData, String> {
    match fetch_yahoo(symbol).await {
        Ok(d) => return Ok(d),
        Err(e) => log::warn!("yahoo: {e}"),
    }
    if !api_key.is_empty() {
        match fetch_finnhub(symbol, api_key).await {
            Ok(d) => return Ok(d),
            Err(e) => log::warn!("finnhub: {e}"),
        }
        match fetch_alphavantage(symbol, api_key).await {
            Ok(d) => return Ok(d),
            Err(e) => log::warn!("alphavantage: {e}"),
        }
    }
    Ok(StockData {
        symbol: symbol.to_string(),
        display_text: format!("📈 {}: Error fetching data", symbol),
        price_string: "---".to_string(),
        change_string: "---".to_string(),
        ..Default::default()
    })
}

/// Fetches 7 days of hourly closes for the hover chart (Yahoo Finance).
pub async fn fetch_chart(symbol: &str) -> Vec<f64> {
    let url =
        format!("https://query1.finance.yahoo.com/v8/finance/chart/{symbol}?interval=1h&range=7d");
    let client = match http() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let resp = match client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let v: Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    v["chart"]["result"][0]["indicators"]["quote"][0]["close"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|c| c.as_f64()).collect())
        .unwrap_or_default()
}

async fn fetch_yahoo(symbol: &str) -> Result<StockData, String> {
    let url = format!("https://query1.finance.yahoo.com/v8/finance/chart/{symbol}");
    let resp = http()?
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36")
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    let meta = &v["chart"]["result"][0]["meta"];
    let price = meta["regularMarketPrice"].as_f64().unwrap_or(0.0);
    if price == 0.0 {
        return Err(format!("invalid price for {symbol}"));
    }
    let mut prev = meta["previousClose"].as_f64().unwrap_or(0.0);
    if prev == 0.0 {
        prev = meta["chartPreviousClose"].as_f64().unwrap_or(0.0);
    }
    let change = price - prev;
    let change_percent = if prev != 0.0 { change / prev * 100.0 } else { 0.0 };
    Ok(format_stock(symbol, price, change, change_percent))
}

async fn fetch_finnhub(symbol: &str, api_key: &str) -> Result<StockData, String> {
    let url = format!("https://finnhub.io/api/v1/quote?symbol={symbol}&token={api_key}");
    let v: Value = http()?.get(&url).send().await.map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    let c = v["c"].as_f64().unwrap_or(0.0);
    if c == 0.0 {
        return Err(format!("no data for {symbol}"));
    }
    Ok(format_stock(symbol, c, v["d"].as_f64().unwrap_or(0.0), v["dp"].as_f64().unwrap_or(0.0)))
}

async fn fetch_alphavantage(symbol: &str, api_key: &str) -> Result<StockData, String> {
    let url = format!("https://www.alphavantage.co/query?function=GLOBAL_QUOTE&symbol={symbol}&apikey={api_key}");
    let v: Value = http()?.get(&url).send().await.map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    let q = &v["Global Quote"];
    if q["01. symbol"].as_str().unwrap_or("").is_empty() {
        return Err(format!("no data for {symbol}"));
    }
    let price = q["05. price"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
    let change = q["09. change"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
    let cp = q["10. change percent"]
        .as_str()
        .unwrap_or("0")
        .trim_end_matches('%')
        .parse::<f64>()
        .unwrap_or(0.0);
    Ok(format_stock(symbol, price, change, cp))
}
