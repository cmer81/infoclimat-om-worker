use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::Deserialize;

use crate::{AppState, aggregate, error::AppError};

pub async fn healthz() -> &'static str {
    "ok"
}

/// Resolved aggregation request — used internally by both handlers.
#[derive(Debug, Clone)]
pub struct AggregateRequest {
    pub domain: String,
    pub base_variable: String,
    pub output_variable: String,
    pub run: DateTime<Utc>,
    pub time: DateTime<Utc>,
    pub hours: u32,
}

// ---------- /v1/sum (query-only, used for direct API testing) ----------

#[derive(Debug, Deserialize)]
pub struct SumQueryParams {
    pub domain: String,
    pub variable: String,
    pub run: DateTime<Utc>,
    pub time: DateTime<Utc>,
    pub hours: u32,
    /// Optional: name of the child in the output OMfile. Defaults to "{variable}_sum_{hours}h".
    pub output_variable: Option<String>,
}

pub async fn sum_query(
    State(state): State<Arc<AppState>>,
    Query(p): Query<SumQueryParams>,
) -> Result<impl IntoResponse, AppError> {
    let req = AggregateRequest {
        domain: p.domain,
        base_variable: p.variable.clone(),
        output_variable: p
            .output_variable
            .unwrap_or_else(|| format!("{}_sum_{}h", p.variable, p.hours)),
        run: p.run,
        time: p.time,
        hours: p.hours,
    };
    serve(state, req).await
}

// ---------- /v1/sum/:domain/:base_var/:hours/:y/:m/:d/:hhmm/:filename (path-style for omProtocol) ----------

#[derive(Debug, Deserialize)]
pub struct SumPath {
    pub domain: String,
    pub base_variable: String,
    pub hours_segment: String, // e.g. "24h"
    pub run_year: i32,
    pub run_month: u32,
    pub run_day: u32,
    pub run_hhmm: String, // e.g. "0000Z"
    pub time_filename: String, // e.g. "2026-05-22T0300.om"
}

pub async fn sum_path(
    State(state): State<Arc<AppState>>,
    Path(p): Path<SumPath>,
) -> Result<impl IntoResponse, AppError> {
    let hours: u32 = p
        .hours_segment
        .strip_suffix('h')
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| AppError::BadRequest(format!("invalid hours segment '{}'", p.hours_segment)))?;

    let run_hhmm = p
        .run_hhmm
        .strip_suffix('Z')
        .ok_or_else(|| AppError::BadRequest(format!("invalid run hhmm '{}'", p.run_hhmm)))?;
    if run_hhmm.len() != 4 {
        return Err(AppError::BadRequest(format!(
            "invalid run hhmm '{}'",
            p.run_hhmm
        )));
    }
    let run_hour: u32 = run_hhmm[..2]
        .parse()
        .map_err(|_| AppError::BadRequest("invalid run hour".into()))?;
    let run_minute: u32 = run_hhmm[2..]
        .parse()
        .map_err(|_| AppError::BadRequest("invalid run minute".into()))?;

    let run = Utc
        .with_ymd_and_hms(p.run_year, p.run_month, p.run_day, run_hour, run_minute, 0)
        .single()
        .ok_or_else(|| AppError::BadRequest("invalid run datetime".into()))?;

    let time_str = p
        .time_filename
        .strip_suffix(".om")
        .ok_or_else(|| AppError::BadRequest(format!("expected .om filename: {}", p.time_filename)))?;
    let time = parse_time_filename(time_str)
        .ok_or_else(|| AppError::BadRequest(format!("invalid time filename: {time_str}")))?;

    let req = AggregateRequest {
        domain: p.domain,
        base_variable: p.base_variable.clone(),
        output_variable: format!("{}_sum_{}h", p.base_variable, hours),
        run,
        time,
        hours,
    };
    serve(state, req).await
}

fn parse_time_filename(s: &str) -> Option<DateTime<Utc>> {
    // Format: YYYY-MM-DDTHHMM
    if s.len() != 15 {
        return None;
    }
    let date = NaiveDate::parse_from_str(&s[..10], "%Y-%m-%d").ok()?;
    let hour: u32 = s[11..13].parse().ok()?;
    let minute: u32 = s[13..15].parse().ok()?;
    Utc.from_local_datetime(&date.and_hms_opt(hour, minute, 0)?)
        .single()
}

// ---------- Shared serve logic ----------

async fn serve(state: Arc<AppState>, req: AggregateRequest) -> Result<impl IntoResponse, AppError> {
    validate(&req)?;

    let key = aggregate::cache_key(&req);

    if let Some(bytes) = state.cache.get(&key).await.map_err(AppError::Cache)? {
        tracing::info!(%key, "cache hit");
        return Ok(om_response(bytes, true));
    }

    tracing::info!(%key, "cache miss, aggregating");
    let bytes = aggregate::compute_sum(&state, &req).await?;
    state
        .cache
        .put(&key, &bytes)
        .await
        .map_err(AppError::Cache)?;

    Ok(om_response(bytes, false))
}

fn validate(r: &AggregateRequest) -> Result<(), AppError> {
    if r.hours == 0 || r.hours > 24 * 14 {
        return Err(AppError::BadRequest(
            "hours must be in 1..=336 (max 14 days)".into(),
        ));
    }
    if r.time < r.run {
        return Err(AppError::BadRequest("time must be >= run".into()));
    }
    if r.domain.is_empty() || r.base_variable.is_empty() {
        return Err(AppError::BadRequest("domain and variable required".into()));
    }
    Ok(())
}

fn om_response(bytes: bytes::Bytes, cache_hit: bool) -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    headers.insert(
        "x-cache",
        HeaderValue::from_static(if cache_hit { "HIT" } else { "MISS" }),
    );
    (StatusCode::OK, headers, bytes)
}
