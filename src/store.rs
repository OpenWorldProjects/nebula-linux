//! Store deals, offers, catalog fetching from GOG.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreItem {
    pub id: String,
    pub title: String,
    pub price: String,
    pub base_price: String,
    pub discount: String,
    pub cover: String,
}

pub async fn fetch_deals(only_discounted: bool, sort: &str, query: &str) -> Result<Vec<StoreItem>, String> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build().map_err(|e| e.to_string())?;
    let mut url = format!(
        "https://catalog.gog.com/v1/catalog?limit=40&locale=en-US&order={sort}&productType=in:game,pack&page=1"
    );
    if only_discounted {
        url.push_str("&discounted=eq:true");
    }
    let query = query.trim();
    if !query.is_empty() {
        let encoded = urlencoding::encode(query);
        url.push_str(&format!("&query=like:{encoded}"));
    }

    let resp = client.get(&url).header("User-Agent", "Nebula/0.1").send().await
        .and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?;
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    if !body["products"].is_array() { return Err("Unexpected catalog response".into()); }

    let mut out = Vec::new();
    if let Some(items) = body["products"].as_array() {
        for p in items {
            let title = p["title"].as_str().unwrap_or("Untitled").to_string();
            let id = p["slug"].as_str().unwrap_or_else(|| p["title"].as_str().unwrap_or("")).to_string();
            let cover = p["coverHorizontal"].as_str().or_else(|| p["coverVertical"].as_str()).unwrap_or("").to_string();
            let price = p["price"]["final"].as_str().unwrap_or("Free").to_string();
            let base_price = p["price"]["base"].as_str().unwrap_or("").to_string();
            let discount = p["price"]["discount"].as_str().unwrap_or("").to_string();

            out.push(StoreItem {
                id,
                title,
                price,
                base_price,
                discount,
                cover,
            });
        }
    }
    Ok(out)
}
