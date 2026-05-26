use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub listen_addr: String,
    pub openmeteo_base_url: String,
    pub max_concurrent_fetches: usize,
    /// Public base URL of the R2 bucket (e.g. `https://pub-xxx.r2.dev` or a
    /// custom domain). When set, OMfile responses are returned as a 307
    /// redirect to `<r2_public_url>/<key>` instead of inline bytes. R2 then
    /// serves the file with native `Range` support, which the JS OM file
    /// reader requires. When unset, we fall back to inline bytes — convenient
    /// for local dev before the bucket is made public.
    pub r2_public_url: Option<String>,
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
            r2_public_url: env::var("R2_PUBLIC_URL")
                .ok()
                .map(|s| s.trim().trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty()),
        })
    }
}
