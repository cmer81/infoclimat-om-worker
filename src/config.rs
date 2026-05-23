use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub listen_addr: String,
    pub openmeteo_base_url: String,
    pub max_concurrent_fetches: usize,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            listen_addr: env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            openmeteo_base_url: env::var("OPENMETEO_BASE_URL")
                .unwrap_or_else(|_| "https://map-tiles.open-meteo.com".to_string()),
            max_concurrent_fetches: env::var("MAX_CONCURRENT_FETCHES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(16),
        })
    }
}
