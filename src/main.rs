use std::sync::Arc;

use axum::{Router, http::Method, routing::get};
use tower_http::{
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

mod aggregate;
mod cache;
mod config;
mod error;
mod grid;
mod handlers;
mod labels;

use config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env from CWD if present — silent if missing so production stays
    // env-driven only.
    let _ = dotenvy::dotenv();

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fmt::layer().json())
        .init();

    let config = Config::from_env()?;
    let listen_addr = config.listen_addr.clone();

    let state = AppState {
        config: Arc::new(config),
        http: reqwest::Client::builder()
            .user_agent(concat!("infoclimat-om-worker/", env!("CARGO_PKG_VERSION")))
            .build()?,
        cache: cache::S3Cache::from_env().await?,
    };

    let app = Router::new()
        .route("/healthz", get(handlers::healthz))
        .route("/v1/sum", get(handlers::sum_query))
        .route(
            "/v1/sum/:domain/:base_variable/:hours_segment/:run_year/:run_month/:run_day/:run_hhmm/:time_filename",
            get(handlers::sum_path),
        )
        .route(
            "/v1/sum_since_0h/:domain/:base_variable/:run_year/:run_month/:run_day/:run_hhmm/:time_filename",
            get(handlers::sum_since_0h_path),
        )
        .route(
            "/v1/labels/:domain/:variable/:run_year/:run_month/:run_day/:run_hhmm/:time/:z/:x/:y_filename",
            get(handlers::labels_path),
        )
        .route("/v1/tile-proxy/*path", get(handlers::tile_proxy))
        .with_state(Arc::new(state))
        .layer(TraceLayer::new_for_http())
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([Method::GET])
                .allow_headers(Any),
        );

    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    tracing::info!(%listen_addr, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}

pub struct AppState {
    pub config: Arc<Config>,
    pub http: reqwest::Client,
    pub cache: cache::S3Cache,
}
