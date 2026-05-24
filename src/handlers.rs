use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
};
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Timelike, Utc};
use serde::Deserialize;

use crate::{AppState, aggregate, error::AppError, labels};

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

// ---------- /v1/sum_since_0h/:domain/:base_variable/:run_year/:run_month/:run_day/:run_hhmm/:time_filename ----------

#[derive(Debug, Deserialize)]
pub struct SumSince0hPath {
    pub domain: String,
    pub base_variable: String,
    pub run_year: i32,
    pub run_month: u32,
    pub run_day: u32,
    pub run_hhmm: String,
    pub time_filename: String,
}

pub async fn sum_since_0h_path(
    State(state): State<Arc<AppState>>,
    Path(p): Path<SumSince0hPath>,
) -> Result<impl IntoResponse, AppError> {
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

    let hours = validate_since_0h(run, time)?;

    let req = AggregateRequest {
        domain: p.domain,
        base_variable: p.base_variable.clone(),
        output_variable: format!("{}_sum_since_0h", p.base_variable),
        run,
        time,
        hours,
    };
    serve(state, req).await
}

// ---------- /v1/labels/:domain/:variable/:run_year/:run_month/:run_day/:run_hhmm/:time/:z/:x/:y.json ----------

#[derive(Debug, Deserialize)]
pub struct LabelsPath {
    pub domain: String,
    pub variable: String,
    pub run_year: i32,
    pub run_month: u32,
    pub run_day: u32,
    pub run_hhmm: String, // e.g. "0000Z"
    pub time: String,     // e.g. "2026-05-25T1500"
    pub z: u8,
    pub x: u32,
    pub y_filename: String, // e.g. "22.json"
}

pub async fn labels_path(
    State(state): State<Arc<AppState>>,
    Path(p): Path<LabelsPath>,
) -> Result<impl IntoResponse, AppError> {
    let run = parse_run_hhmm(p.run_year, p.run_month, p.run_day, &p.run_hhmm)?;
    let time = parse_time_filename(&p.time)
        .ok_or_else(|| AppError::BadRequest(format!("invalid time '{}'", p.time)))?;

    let y_str = p
        .y_filename
        .strip_suffix(".json")
        .ok_or_else(|| AppError::BadRequest(format!("expected .json suffix on y: {}", p.y_filename)))?;
    let y: u32 = y_str
        .parse()
        .map_err(|_| AppError::BadRequest(format!("invalid y '{}'", y_str)))?;

    if p.z > 8 {
        return Err(AppError::BadRequest(format!(
            "z={} out of range [0,8]", p.z
        )));
    }
    let max_xy = 1u32 << p.z;
    if p.x >= max_xy || y >= max_xy {
        return Err(AppError::BadRequest(format!(
            "x={} or y={} out of range for z={} (max {})",
            p.x, y, p.z, max_xy
        )));
    }

    let req = labels::LabelsRequest {
        domain: p.domain,
        variable: p.variable,
        run,
        time,
        z: p.z,
        x: p.x,
        y,
    };

    let key = labels::cache_key(&req);
    if let Some(bytes) = state.cache.get(&key).await.map_err(AppError::Cache)? {
        tracing::info!(%key, "labels cache hit");
        return Ok(json_response(bytes, true));
    }
    tracing::info!(%key, "labels cache miss, computing");
    let bytes = labels::compute_labels(&state, &req).await?;
    state.cache.put(&key, &bytes).await.map_err(AppError::Cache)?;
    Ok(json_response(bytes, false))
}

fn parse_run_hhmm(year: i32, month: u32, day: u32, hhmm: &str) -> Result<DateTime<Utc>, AppError> {
    let trimmed = hhmm
        .strip_suffix('Z')
        .ok_or_else(|| AppError::BadRequest(format!("invalid run hhmm '{}'", hhmm)))?;
    if trimmed.len() != 4 {
        return Err(AppError::BadRequest(format!("invalid run hhmm '{}'", hhmm)));
    }
    let h: u32 = trimmed[..2]
        .parse()
        .map_err(|_| AppError::BadRequest("invalid run hour".into()))?;
    let m: u32 = trimmed[2..]
        .parse()
        .map_err(|_| AppError::BadRequest("invalid run minute".into()))?;
    Utc.with_ymd_and_hms(year, month, day, h, m, 0)
        .single()
        .ok_or_else(|| AppError::BadRequest("invalid run datetime".into()))
}

fn json_response(bytes: bytes::Bytes, cache_hit: bool) -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
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
    // The cumul window is [time - hours + 1, time]. Open-Meteo's spatial bucket
    // only contains the *forecast* hours of a run (H+0 onwards), never the
    // hours preceding the run, so a window whose start is before `run` would
    // 404 on every step before it. Reject with a clear 400 instead of letting
    // it explode into an upstream 502.
    let window_start = r.time - Duration::hours(r.hours as i64 - 1);
    if window_start < r.run {
        return Err(AppError::BadRequest(format!(
            "cumul window starts at {} but run is {} — a run does not contain its past; \
             pick a run at or before {} (e.g. an earlier model_run)",
            window_start.to_rfc3339(),
            r.run.to_rfc3339(),
            window_start.to_rfc3339(),
        )));
    }
    Ok(())
}

/// Validates that a (run, time) pair is admissible for the "sum since 00 UTC"
/// endpoint: the run must be at 00:00 UTC of the calendar day containing `time`.
/// Returns the number of source steps to aggregate (i.e. `time.hour() + 1`).
pub fn validate_since_0h(run: DateTime<Utc>, time: DateTime<Utc>) -> Result<u32, AppError> {
    if run.hour() != 0 || run.minute() != 0 || run.second() != 0 {
        return Err(AppError::BadRequest(format!(
            "sum_since_0h requires run at 00:00 UTC, got {}",
            run.to_rfc3339()
        )));
    }
    if run.date_naive() != time.date_naive() {
        return Err(AppError::BadRequest(format!(
            "sum_since_0h requires run and time on the same UTC day, got run={} time={}",
            run.date_naive(),
            time.date_naive()
        )));
    }
    Ok(time.hour() + 1)
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

// ---------- /v1/tile-proxy/*path — CORS proxy for tiles.open-meteo.com ----------
//
// Upstream basemap host (tiles.open-meteo.com) doesn't serve CORS headers
// (likely an oversight — their other hosts do). This proxy forwards GETs and
// rewrites TileJSON so MapLibre keeps hitting us for the .mvt tiles too.
// Allowlist on path extension keeps it from being abused as an open proxy.

pub async fn tile_proxy(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(path): Path<String>,
) -> Result<axum::response::Response, AppError> {
    let allowed = (path.ends_with(".json") || path.ends_with(".mvt") || path.ends_with(".pbf"))
        && !path.contains("..");
    if !allowed {
        return Err(AppError::BadRequest(format!(
            "tile-proxy: path not allowed: {path}"
        )));
    }

    let upstream = format!("https://tiles.open-meteo.com/{path}");
    let resp = state
        .http
        .get(&upstream)
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("tile-proxy fetch {upstream}: {e}")))?;

    let status = resp.status();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let body = resp
        .bytes()
        .await
        .map_err(|e| AppError::Upstream(format!("tile-proxy body: {e}")))?;

    // TileJSON references absolute .mvt URLs back on tiles.open-meteo.com.
    // Rewrite them to point at us, otherwise MapLibre fetches the tiles
    // directly and hits the same CORS wall.
    let final_body: bytes::Bytes = if path.ends_with(".json") {
        let public_base = derive_public_base(&headers);
        let text = String::from_utf8_lossy(&body);
        let rewritten = text.replace(
            "https://tiles.open-meteo.com/",
            &format!("{public_base}/v1/tile-proxy/"),
        );
        bytes::Bytes::from(rewritten.into_bytes())
    } else {
        body
    };

    let mut builder = axum::response::Response::builder().status(status);
    if let Some(ct) = content_type {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    builder = builder.header(header::CACHE_CONTROL, "public, max-age=86400");
    builder
        .body(axum::body::Body::from(final_body))
        .map_err(|e| AppError::Internal(anyhow::anyhow!("tile-proxy build response: {e}")))
}

fn derive_public_base(headers: &HeaderMap) -> String {
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost:8080");
    format!("{proto}://{host}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn validate_since_0h_ok() {
        let run = Utc.with_ymd_and_hms(2026, 5, 23, 0, 0, 0).unwrap();
        let time = Utc.with_ymd_and_hms(2026, 5, 23, 15, 0, 0).unwrap();
        assert_eq!(validate_since_0h(run, time).unwrap(), 16);
    }

    #[test]
    fn validate_since_0h_rejects_non_00z_run() {
        let run = Utc.with_ymd_and_hms(2026, 5, 23, 12, 0, 0).unwrap();
        let time = Utc.with_ymd_and_hms(2026, 5, 23, 18, 0, 0).unwrap();
        let err = validate_since_0h(run, time).unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn validate_since_0h_rejects_cross_day() {
        let run = Utc.with_ymd_and_hms(2026, 5, 22, 0, 0, 0).unwrap();
        let time = Utc.with_ymd_and_hms(2026, 5, 23, 3, 0, 0).unwrap();
        let err = validate_since_0h(run, time).unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn validate_since_0h_at_midnight_returns_one_step() {
        let run = Utc.with_ymd_and_hms(2026, 5, 23, 0, 0, 0).unwrap();
        let time = Utc.with_ymd_and_hms(2026, 5, 23, 0, 0, 0).unwrap();
        assert_eq!(validate_since_0h(run, time).unwrap(), 1);
    }

    fn agg(hours: u32, run: DateTime<Utc>, time: DateTime<Utc>) -> AggregateRequest {
        AggregateRequest {
            domain: "meteofrance_arome_france_hd".into(),
            base_variable: "precipitation".into(),
            output_variable: format!("precipitation_sum_{hours}h"),
            run,
            time,
            hours,
        }
    }

    #[test]
    fn validate_rejects_window_starting_before_run() {
        // Real-world failure: 24h cumul ending at 22:00 of run 15:00 needs the
        // 23:00 of the previous day, which doesn't exist in this run's bucket.
        let run = Utc.with_ymd_and_hms(2026, 5, 24, 15, 0, 0).unwrap();
        let time = Utc.with_ymd_and_hms(2026, 5, 24, 22, 0, 0).unwrap();
        let err = validate(&agg(24, run, time)).unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn validate_accepts_window_exactly_aligned_with_run() {
        // 24h cumul ending at H+23 of a 00z run is the tightest legal window.
        let run = Utc.with_ymd_and_hms(2026, 5, 24, 0, 0, 0).unwrap();
        let time = Utc.with_ymd_and_hms(2026, 5, 24, 23, 0, 0).unwrap();
        validate(&agg(24, run, time)).unwrap();
    }

    #[test]
    fn validate_accepts_window_fully_after_run() {
        let run = Utc.with_ymd_and_hms(2026, 5, 24, 0, 0, 0).unwrap();
        let time = Utc.with_ymd_and_hms(2026, 5, 25, 12, 0, 0).unwrap();
        validate(&agg(24, run, time)).unwrap();
    }
}
