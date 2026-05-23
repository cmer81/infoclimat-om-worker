use std::sync::Arc;

use bytes::Bytes;
use chrono::{DateTime, Duration, Utc};
use futures::{StreamExt, stream::FuturesUnordered};
use ndarray::ArrayD;
use omfiles::{
    InMemoryBackend, OmCompressionType,
    reader::OmFileReader,
    traits::{OmArrayVariable, OmFileReadable},
    writer::OmFileWriter,
};
use sha2::{Digest, Sha256};

use crate::{AppState, error::AppError, handlers::AggregateRequest};

/// Stable cache key for an aggregation request.
pub fn cache_key(r: &AggregateRequest) -> String {
    let mut h = Sha256::new();
    h.update(r.domain.as_bytes());
    h.update(b"|");
    h.update(r.base_variable.as_bytes());
    h.update(b"|");
    h.update(r.output_variable.as_bytes());
    h.update(b"|");
    h.update(r.run.to_rfc3339().as_bytes());
    h.update(b"|");
    h.update(r.time.to_rfc3339().as_bytes());
    h.update(b"|");
    h.update(r.hours.to_le_bytes());
    format!(
        "v1/{}/{}/{}h/{}.om",
        r.domain,
        r.output_variable,
        r.hours,
        hex::encode(&h.finalize()[..16])
    )
}

/// Builds the upstream URL for one hourly OMfile, matching the schema produced
/// by getOMUrl() in maps/src/lib/url.ts:
///   {base}/data_spatial/{domain}/{YYYY}/{MM}/{DD}/{HH}{mm}Z/{YYYY}-{MM}-{DD}T{HH}{mm}.om
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

pub async fn compute_sum(state: &Arc<AppState>, r: &AggregateRequest) -> Result<Bytes, AppError> {
    let steps = source_steps(r);

    let mut tasks: FuturesUnordered<_> = steps
        .iter()
        .map(|step_time| fetch_one(state.clone(), r.clone(), *step_time))
        .collect();

    let mut grids: Vec<DecodedGrid> = Vec::with_capacity(steps.len());
    while let Some(res) = tasks.next().await {
        grids.push(res?);
    }

    let summed = sum_grids(grids)?;
    encode_omfile(&r.output_variable, &summed)
}

fn source_steps(r: &AggregateRequest) -> Vec<DateTime<Utc>> {
    let end = r.time;
    let start = end - Duration::hours(r.hours as i64 - 1);
    (0..r.hours)
        .map(|i| start + Duration::hours(i as i64))
        .collect()
}

async fn fetch_one(
    state: Arc<AppState>,
    r: AggregateRequest,
    step_time: DateTime<Utc>,
) -> Result<DecodedGrid, AppError> {
    let url = source_url(&state.config.openmeteo_base_url, &r.domain, r.run, step_time);
    tracing::debug!(%url, "fetching source omfile");

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

    decode_grid(bytes.to_vec(), &r.base_variable).map_err(AppError::Aggregate)
}

fn decode_grid(bytes: Vec<u8>, variable: &str) -> Result<DecodedGrid, String> {
    let backend = Arc::new(InMemoryBackend::new(bytes));
    let root = OmFileReader::new(backend).map_err(|e| format!("open omfile: {e}"))?;

    let var = root
        .get_child_by_name(variable)
        .ok_or_else(|| format!("variable '{variable}' not found in source omfile"))?;

    let arr = var
        .expect_array()
        .map_err(|e| format!("variable '{variable}' is not an array: {e}"))?;

    let scale = arr.scale_factor();
    let offset = arr.add_offset();
    let compression = arr.compression();
    let dimensions: Vec<u64> = arr.get_dimensions().to_vec();
    let chunk_dimensions: Vec<u64> = arr.get_chunk_dimensions().to_vec();

    let full_range: Vec<std::ops::Range<u64>> = dimensions.iter().map(|&d| 0..d).collect();
    let data: ArrayD<f32> = arr
        .read::<f32>(&full_range)
        .map_err(|e| format!("read array '{variable}': {e}"))?;

    Ok(DecodedGrid {
        data,
        dimensions,
        chunk_dimensions,
        scale,
        offset,
        compression,
    })
}

fn sum_grids(grids: Vec<DecodedGrid>) -> Result<DecodedGrid, AppError> {
    let mut iter = grids.into_iter();
    let mut acc = iter
        .next()
        .ok_or_else(|| AppError::Aggregate("no source grids".into()))?;

    for g in iter {
        if g.dimensions != acc.dimensions {
            return Err(AppError::Aggregate(format!(
                "dimension mismatch: {:?} vs {:?}",
                acc.dimensions, g.dimensions
            )));
        }
        let acc_slice = acc
            .data
            .as_slice_mut()
            .ok_or_else(|| AppError::Aggregate("accumulator not contiguous".into()))?;
        let next_slice = g
            .data
            .as_slice()
            .ok_or_else(|| AppError::Aggregate("addend not contiguous".into()))?;

        for (a, b) in acc_slice.iter_mut().zip(next_slice.iter()) {
            // NaN propagates: if either source pixel is missing, the cumul is missing.
            *a += *b;
        }
    }
    Ok(acc)
}

fn encode_omfile(variable: &str, grid: &DecodedGrid) -> Result<Bytes, AppError> {
    use std::borrow::BorrowMut;

    let mut backend = InMemoryBackend::new(Vec::with_capacity(1 << 20));
    let mut writer = OmFileWriter::new(backend.borrow_mut(), 1 << 20);

    let finalized = {
        let mut arr_writer = writer
            .prepare_array::<f32>(
                grid.dimensions.clone(),
                grid.chunk_dimensions.clone(),
                grid.compression,
                grid.scale,
                grid.offset,
            )
            .map_err(|e| AppError::Aggregate(format!("prepare_array: {e}")))?;

        arr_writer
            .write_data(grid.data.view(), None, None)
            .map_err(|e| AppError::Aggregate(format!("write_data: {e}")))?;

        arr_writer.finalize()
    };

    let root_offset = writer
        .write_array(finalized, variable, &[])
        .map_err(|e| AppError::Aggregate(format!("write_array root: {e}")))?;

    writer
        .write_trailer(root_offset)
        .map_err(|e| AppError::Aggregate(format!("write_trailer: {e}")))?;

    drop(writer);

    // InMemoryBackend keeps the buffer private; pull it back through the
    // public reader interface (cheap — single copy of a sub-MB buffer).
    use omfiles::traits::OmFileReaderBackend;
    let n = backend.count();
    let raw = backend
        .get_bytes(0, n as u64)
        .map(|b| b.to_vec())
        .map_err(|e| AppError::Aggregate(format!("extract bytes: {e}")))?;
    Ok(Bytes::from(raw))
}

pub struct DecodedGrid {
    pub data: ArrayD<f32>,
    pub dimensions: Vec<u64>,
    pub chunk_dimensions: Vec<u64>,
    pub scale: f32,
    pub offset: f32,
    pub compression: OmCompressionType,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn req(hours: u32, time: DateTime<Utc>, run: DateTime<Utc>) -> AggregateRequest {
        AggregateRequest {
            domain: "meteofrance_arome_france_hd".into(),
            base_variable: "precipitation".into(),
            output_variable: "precipitation_sum_since_0h".into(),
            run,
            time,
            hours,
        }
    }

    #[test]
    fn source_steps_since_0h_at_15z_returns_16_grids_from_00z() {
        let run = Utc.with_ymd_and_hms(2026, 5, 23, 0, 0, 0).unwrap();
        let time = Utc.with_ymd_and_hms(2026, 5, 23, 15, 0, 0).unwrap();
        let steps = source_steps(&req(16, time, run));
        assert_eq!(steps.len(), 16);
        assert_eq!(steps[0], Utc.with_ymd_and_hms(2026, 5, 23, 0, 0, 0).unwrap());
        assert_eq!(steps[15], time);
    }
}
