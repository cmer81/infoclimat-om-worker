// Per-tile label rendering: turn a spatial OMfile into a thinned GeoJSON
// FeatureCollection of grid-cell-center points, so the maps client can render
// numeric labels on top of the raster (Meteociel-style).
//
// Self-describing pipeline — the OMfile carries everything we need:
//   - root child `crs_wkt`   → grid bbox (via grid::Grid)
//   - target variable        → array dims + values (f32)
//   - target variable's      → unit (string scalar)
//     `unit` child

use std::sync::Arc;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use omfiles::{
    InMemoryBackend,
    reader::OmFileReader,
    traits::{OmArrayVariable, OmFileReadable, OmScalarVariable},
};
use serde::Serialize;

use crate::{
    AppState,
    error::AppError,
    grid::{self, Grid, sampling_step, tile_to_bbox},
};

/// Resolved request — the handler shapes path params into this.
#[derive(Debug, Clone)]
pub struct LabelsRequest {
    pub domain: String,
    pub variable: String,
    pub run: DateTime<Utc>,
    pub time: DateTime<Utc>,
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

/// Cache key for an XYZ-tiled labels response. Stable & deterministic — a past
/// run is immutable so this entry can be cached forever.
pub fn cache_key(r: &LabelsRequest) -> String {
    format!(
        "v1/labels/{}/{}/{}/{}/z{}/x{}/y{}.json",
        r.domain,
        r.variable,
        r.run.format("%Y-%m-%dT%H%MZ"),
        r.time.format("%Y-%m-%dT%H%MZ"),
        r.z,
        r.x,
        r.y,
    )
}

/// Soft cap on labels emitted along the longer axis of the visible tile.
/// Keeps MapLibre symbol count tractable (~thousands max per layer).
const MAX_LABELS_PER_SIDE: u32 = 32;

pub async fn compute_labels(state: &Arc<AppState>, r: &LabelsRequest) -> Result<Bytes, AppError> {
    let url = source_url(&state.config.openmeteo_base_url, &r.domain, r.run, r.time);
    tracing::debug!(%url, "fetching spatial omfile for labels");
    let resp = state
        .http
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("GET {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Upstream(format!(
            "GET {url}: HTTP {}",
            resp.status()
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::Upstream(format!("read body {url}: {e}")))?;

    let fc = decode_and_sample(bytes.to_vec(), r)?;
    let json = serde_json::to_vec(&fc)
        .map_err(|e| AppError::Aggregate(format!("serialize labels json: {e}")))?;
    Ok(Bytes::from(json))
}

fn source_url(base: &str, domain: &str, run: DateTime<Utc>, time: DateTime<Utc>) -> String {
    let run_path = run.format("%Y/%m/%d/%H%MZ");
    let time_file = time.format("%Y-%m-%dT%H%M");
    format!(
        "{}/data_spatial/{}/{}/{}.om",
        base.trim_end_matches('/'),
        domain,
        run_path,
        time_file
    )
}

fn decode_and_sample(bytes: Vec<u8>, r: &LabelsRequest) -> Result<LabelsResponse, AppError> {
    let backend = Arc::new(InMemoryBackend::new(bytes));
    let root =
        OmFileReader::new(backend).map_err(|e| AppError::Aggregate(format!("open omfile: {e}")))?;

    let wkt = root
        .get_child_by_name("crs_wkt")
        .and_then(|c| c.expect_scalar().ok().and_then(|s| s.read_scalar::<String>()))
        .ok_or_else(|| AppError::Aggregate("crs_wkt scalar not found at root".into()))?;

    let var = root
        .get_child_by_name(&r.variable)
        .ok_or_else(|| AppError::BadRequest(format!("variable '{}' not found", r.variable)))?;

    let arr = var
        .expect_array()
        .map_err(|e| AppError::Aggregate(format!("variable '{}' not an array: {e}", r.variable)))?;

    let dims = arr.get_dimensions();
    if dims.len() != 2 {
        return Err(AppError::Aggregate(format!(
            "expected 2D variable, got dims={dims:?}"
        )));
    }
    let rows = dims[0] as u32;
    let cols = dims[1] as u32;

    let unit = var
        .get_child_by_name("unit")
        .and_then(|c| c.expect_scalar().ok().and_then(|s| s.read_scalar::<String>()));

    let grid_g = Grid::from_wkt(&wkt, rows, cols)?;
    let tile_bbox = tile_to_bbox(r.z, r.x, r.y);
    let range = match grid_g.cells_in_bbox(grid::LatLonBBox {
        south: tile_bbox.south,
        west: tile_bbox.west,
        north: tile_bbox.north,
        east: tile_bbox.east,
    }) {
        Some(r) => r,
        None => {
            return Ok(LabelsResponse::empty(r, unit, grid_g));
        }
    };

    let step = sampling_step(range, MAX_LABELS_PER_SIDE);

    let i_lo = range.i_min as u64;
    let i_hi = (range.i_max + 1) as u64;
    let j_lo = range.j_min as u64;
    let j_hi = (range.j_max + 1) as u64;
    let sub = arr
        .read::<f32>(&[i_lo..i_hi, j_lo..j_hi])
        .map_err(|e| AppError::Aggregate(format!("read sub-array: {e}")))?;

    let mut features: Vec<Feature> = Vec::new();
    let stride = step as usize;
    let h = (i_hi - i_lo) as usize;
    let w = (j_hi - j_lo) as usize;
    features.reserve((h / stride + 1) * (w / stride + 1));

    for ii in (0..h).step_by(stride) {
        for jj in (0..w).step_by(stride) {
            let v = sub[[ii, jj]];
            if !v.is_finite() {
                continue;
            }
            let i_abs = range.i_min + ii as u32;
            let j_abs = range.j_min + jj as u32;
            let (lat, lon) = grid_g.cell_lat_lon(i_abs, j_abs);
            features.push(Feature {
                feature_type: "Feature",
                geometry: Geometry {
                    geometry_type: "Point",
                    coordinates: [round6(lon), round6(lat)],
                },
                properties: Properties { v: round2(v) },
            });
        }
    }

    Ok(LabelsResponse {
        domain: r.domain.clone(),
        variable: r.variable.clone(),
        unit,
        run: r.run.to_rfc3339(),
        time: r.time.to_rfc3339(),
        grid: GridMeta {
            dx: round6(grid_g.dx()),
            dy: round6(grid_g.dy()),
            rows,
            cols,
        },
        sampling_step: step,
        features: FeatureCollection {
            collection_type: "FeatureCollection",
            features,
        },
    })
}

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}
fn round2(x: f32) -> f32 {
    (x * 100.0).round() / 100.0
}

#[derive(Debug, Serialize)]
pub struct LabelsResponse {
    pub domain: String,
    pub variable: String,
    pub unit: Option<String>,
    pub run: String,
    pub time: String,
    pub grid: GridMeta,
    pub sampling_step: u32,
    pub features: FeatureCollection,
}

impl LabelsResponse {
    fn empty(r: &LabelsRequest, unit: Option<String>, g: Grid) -> Self {
        Self {
            domain: r.domain.clone(),
            variable: r.variable.clone(),
            unit,
            run: r.run.to_rfc3339(),
            time: r.time.to_rfc3339(),
            grid: GridMeta {
                dx: round6(g.dx()),
                dy: round6(g.dy()),
                rows: g.rows,
                cols: g.cols,
            },
            sampling_step: 1,
            features: FeatureCollection {
                collection_type: "FeatureCollection",
                features: Vec::new(),
            },
        }
    }
}

#[derive(Debug, Serialize)]
pub struct GridMeta {
    pub dx: f64,
    pub dy: f64,
    pub rows: u32,
    pub cols: u32,
}

#[derive(Debug, Serialize)]
pub struct FeatureCollection {
    #[serde(rename = "type")]
    pub collection_type: &'static str,
    pub features: Vec<Feature>,
}

#[derive(Debug, Serialize)]
pub struct Feature {
    #[serde(rename = "type")]
    pub feature_type: &'static str,
    pub geometry: Geometry,
    pub properties: Properties,
}

#[derive(Debug, Serialize)]
pub struct Geometry {
    #[serde(rename = "type")]
    pub geometry_type: &'static str,
    /// [lon, lat] per GeoJSON RFC 7946.
    pub coordinates: [f64; 2],
}

#[derive(Debug, Serialize)]
pub struct Properties {
    pub v: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn req(z: u8, x: u32, y: u32) -> LabelsRequest {
        LabelsRequest {
            domain: "meteofrance_arpege_europe".into(),
            variable: "temperature_2m".into(),
            run: Utc.with_ymd_and_hms(2026, 5, 24, 0, 0, 0).unwrap(),
            time: Utc.with_ymd_and_hms(2026, 5, 25, 15, 0, 0).unwrap(),
            z,
            x,
            y,
        }
    }

    #[test]
    fn cache_key_is_stable_and_path_safe() {
        let key = cache_key(&req(6, 32, 22));
        assert_eq!(
            key,
            "v1/labels/meteofrance_arpege_europe/temperature_2m/2026-05-24T0000Z/2026-05-25T1500Z/z6/x32/y22.json"
        );
    }
}
