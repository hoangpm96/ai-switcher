use crate::models::TokenBreakdown;
use crate::quota::curl_get;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// LiteLLM's public pricing dataset (same source ccusage uses). Maps a model name to
/// per-token costs in USD.
const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

/// Re-fetch the pricing file at most once a day.
const PRICE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Per-token USD costs for one model.
#[derive(Clone, Copy, Debug, Default)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_creation: f64,
}

impl ModelPrice {
    /// Cost in USD for a token breakdown.
    pub fn cost(&self, tokens: &TokenBreakdown) -> f64 {
        tokens.input as f64 * self.input
            + tokens.output as f64 * self.output
            + tokens.cache_read as f64 * self.cache_read
            + tokens.cache_creation as f64 * self.cache_creation
    }
}

pub struct PriceTable {
    models: HashMap<String, ModelPrice>,
    /// "live" (just fetched), "cached" (read the on-disk cache), or "unavailable".
    pub status: String,
    /// When the cache file was last written (RFC3339), if any.
    pub updated_at: Option<String>,
}

impl PriceTable {
    fn empty(status: &str) -> Self {
        Self {
            models: HashMap::new(),
            status: status.to_string(),
            updated_at: None,
        }
    }

    /// Look up a model's price, tolerating small name differences between the CLI logs and
    /// LiteLLM keys (provider prefix, trailing `-YYYYMMDD` date).
    pub fn lookup(&self, model: &str) -> Option<ModelPrice> {
        if let Some(price) = self.models.get(model) {
            return Some(*price);
        }
        for prefix in ["anthropic/", "openai/"] {
            if let Some(price) = self.models.get(&format!("{prefix}{model}")) {
                return Some(*price);
            }
        }
        // Strip a leading `provider/` segment (e.g. Codex's "cx/gpt-5.3-codex").
        if let Some((_, rest)) = model.split_once('/') {
            if let Some(price) = self.models.get(rest) {
                return Some(*price);
            }
        }
        // Strip a trailing `-YYYYMMDD` snapshot date (e.g. claude-sonnet-4-5-20250929).
        if let Some(base) = strip_date_suffix(model) {
            if let Some(price) = self.models.get(base) {
                return Some(*price);
            }
        }
        None
    }

    /// Cost for a breakdown, or None if the model has no known price.
    pub fn cost(&self, model: &str, tokens: &TokenBreakdown) -> Option<f64> {
        self.lookup(model).map(|price| price.cost(tokens))
    }

    pub fn is_available(&self) -> bool {
        !self.models.is_empty()
    }
}

/// Load the LiteLLM price table, refreshing the on-disk cache if it's missing or older than
/// 24h. Falls back to the stale cache (then to "unavailable") when offline.
pub fn load_price_table(cache_path: &Path) -> PriceTable {
    let cache_age = cache_path
        .metadata()
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok());
    let fresh = cache_age.is_some_and(|age| age < PRICE_TTL);

    if fresh {
        if let Some(table) = read_cache(cache_path, "cached") {
            return table;
        }
    }

    // Stale or missing → try to fetch a fresh copy.
    match curl_get(LITELLM_URL, &[("Accept", "application/json")]) {
        Ok(body) if serde_json::from_str::<serde_json::Value>(&body).is_ok() => {
            let _ = std::fs::write(cache_path, &body);
            parse_table(&body, "live", file_modified_rfc3339(cache_path))
        }
        _ => read_cache(cache_path, "cached").unwrap_or_else(|| PriceTable::empty("unavailable")),
    }
}

fn read_cache(path: &Path, status: &str) -> Option<PriceTable> {
    let body = std::fs::read_to_string(path).ok()?;
    let table = parse_table(&body, status, file_modified_rfc3339(path));
    table.is_available().then_some(table)
}

fn parse_table(body: &str, status: &str, updated_at: Option<String>) -> PriceTable {
    let value: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(_) => return PriceTable::empty("unavailable"),
    };
    let Some(map) = value.as_object() else {
        return PriceTable::empty("unavailable");
    };

    let mut models = HashMap::new();
    for (name, entry) in map {
        // Skip the metadata key and any model without an input price.
        let Some(input) = entry.get("input_cost_per_token").and_then(|v| v.as_f64()) else {
            continue;
        };
        let price = ModelPrice {
            input,
            output: entry
                .get("output_cost_per_token")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
            cache_read: entry
                .get("cache_read_input_token_cost")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
            cache_creation: entry
                .get("cache_creation_input_token_cost")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0),
        };
        models.insert(name.clone(), price);
    }

    if models.is_empty() {
        return PriceTable::empty("unavailable");
    }
    PriceTable {
        models,
        status: status.to_string(),
        updated_at,
    }
}

fn file_modified_rfc3339(path: &Path) -> Option<String> {
    let modified = path.metadata().ok()?.modified().ok()?;
    let datetime: chrono::DateTime<chrono::Utc> = modified.into();
    Some(datetime.to_rfc3339())
}

/// Returns the model name without a trailing `-YYYYMMDD` snapshot date, if present.
fn strip_date_suffix(model: &str) -> Option<&str> {
    let (base, date) = model.rsplit_once('-')?;
    if date.len() == 8 && date.chars().all(|c| c.is_ascii_digit()) {
        Some(base)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_table() -> PriceTable {
        let body = r#"{
            "sample_spec": {"note": "ignored — has no input_cost_per_token"},
            "claude-sonnet-4-5": {
                "input_cost_per_token": 0.000003,
                "output_cost_per_token": 0.000015,
                "cache_read_input_token_cost": 0.0000003,
                "cache_creation_input_token_cost": 0.00000375
            },
            "gpt-5.5": {
                "input_cost_per_token": 0.00000125,
                "output_cost_per_token": 0.00001,
                "cache_read_input_token_cost": 0.000000125
            }
        }"#;
        parse_table(body, "live", None)
    }

    #[test]
    fn parses_and_skips_metadata_key() {
        let table = sample_table();
        assert!(table.is_available());
        assert!(table.lookup("sample_spec").is_none());
        assert!(table.lookup("gpt-5.5").is_some());
    }

    #[test]
    fn matches_model_with_date_suffix() {
        let table = sample_table();
        // The Claude JSONL logs models like "claude-sonnet-4-5-20250929".
        let price = table
            .lookup("claude-sonnet-4-5-20250929")
            .expect("date-suffixed model resolves to base price");
        assert_eq!(price.input, 0.000003);
    }

    #[test]
    fn computes_cost_from_breakdown() {
        let table = sample_table();
        let tokens = TokenBreakdown {
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 0,
            cache_creation: 0,
        };
        let cost = table.cost("gpt-5.5", &tokens).expect("priced");
        // 1M input * 1.25e-6 + 1M output * 1e-5 = 1.25 + 10.0
        assert!((cost - 11.25).abs() < 1e-9);
    }

    #[test]
    fn unknown_model_has_no_cost() {
        let table = sample_table();
        assert!(table.cost("some-unknown-model", &TokenBreakdown::default()).is_none());
    }
}
