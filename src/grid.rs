// Self-describing geographic grid extracted from an Open-Meteo spatial OMfile.
//
// Spatial OMfiles carry their CRS as a `crs_wkt` scalar at the root level. The
// WKT contains a `BBOX[south, west, north, east]` clause (per ISO 19162) which,
// combined with the array's `[rows, cols]` dimensions (dim0=lat, dim1=lon per
// the `coordinates="lat lon"` scalar), is enough to reconstruct the lat/lon of
// every grid cell. No domain-specific table required.

use crate::error::AppError;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub rows: u32,
    pub cols: u32,
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
}

impl Grid {
    /// Build from WKT BBOX clause + array dimensions.
    pub fn from_wkt(wkt: &str, rows: u32, cols: u32) -> Result<Self, AppError> {
        let (south, west, north, east) = parse_wkt_bbox(wkt)?;
        if rows < 2 || cols < 2 {
            return Err(AppError::BadRequest(format!(
                "grid too small: rows={rows} cols={cols}"
            )));
        }
        Ok(Self {
            rows,
            cols,
            south,
            west,
            north,
            east,
        })
    }

    /// Spacing in degrees between adjacent cells along latitude (dy) and
    /// longitude (dx). Cells are anchored at the BBOX corners, so the last
    /// cell sits exactly on the opposite corner.
    pub fn dy(&self) -> f64 {
        (self.north - self.south) / (self.rows as f64 - 1.0)
    }
    pub fn dx(&self) -> f64 {
        (self.east - self.west) / (self.cols as f64 - 1.0)
    }

    /// Geographic coordinates (lat, lon) of grid cell (i, j).
    /// `i` indexes latitude (0 = south), `j` indexes longitude (0 = west).
    pub fn cell_lat_lon(&self, i: u32, j: u32) -> (f64, f64) {
        (
            self.south + i as f64 * self.dy(),
            self.west + j as f64 * self.dx(),
        )
    }

    /// Inclusive (i, j) range of cells whose centers fall inside the given
    /// lat/lon bbox. Empty range if the bbox doesn't intersect the grid.
    pub fn cells_in_bbox(&self, bbox: LatLonBBox) -> Option<CellRange> {
        let (s, w, n, e) = (bbox.south, bbox.west, bbox.north, bbox.east);
        if e < self.west || w > self.east || n < self.south || s > self.north {
            return None;
        }
        let dy = self.dy();
        let dx = self.dx();

        let i_min = ((s - self.south) / dy).ceil().max(0.0) as i64;
        let i_max = ((n - self.south) / dy).floor().min((self.rows - 1) as f64) as i64;
        let j_min = ((w - self.west) / dx).ceil().max(0.0) as i64;
        let j_max = ((e - self.west) / dx).floor().min((self.cols - 1) as f64) as i64;

        if i_min > i_max || j_min > j_max {
            return None;
        }
        Some(CellRange {
            i_min: i_min as u32,
            i_max: i_max as u32,
            j_min: j_min as u32,
            j_max: j_max as u32,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatLonBBox {
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellRange {
    pub i_min: u32,
    pub i_max: u32,
    pub j_min: u32,
    pub j_max: u32,
}

/// Bounding box of a Web Mercator XYZ tile in degrees. Uses the standard
/// spherical Mercator definition (lat clamped to ±85.0511°).
pub fn tile_to_bbox(z: u8, x: u32, y: u32) -> LatLonBBox {
    let n = 1u32 << z;
    let lon = |xi: f64| xi / n as f64 * 360.0 - 180.0;
    let lat = |yi: f64| {
        let n_pi = std::f64::consts::PI * (1.0 - 2.0 * yi / n as f64);
        n_pi.sinh().atan().to_degrees()
    };
    LatLonBBox {
        west: lon(x as f64),
        east: lon((x + 1) as f64),
        north: lat(y as f64),
        south: lat((y + 1) as f64),
    }
}

/// Sampling step (in grid cells) so that no more than `max_labels_per_side`
/// labels appear along the longer axis of `range`. Floor at 1.
pub fn sampling_step(range: CellRange, max_labels_per_side: u32) -> u32 {
    let h = range.i_max - range.i_min + 1;
    let w = range.j_max - range.j_min + 1;
    let longest = h.max(w);
    longest.div_ceil(max_labels_per_side.max(1)).max(1)
}

fn parse_wkt_bbox(wkt: &str) -> Result<(f64, f64, f64, f64), AppError> {
    // We only need the BBOX clause; do a substring search rather than pull a
    // full WKT parser. Format expected: BBOX[<s>,<w>,<n>,<e>] possibly with
    // whitespace.
    let start = wkt
        .find("BBOX[")
        .ok_or_else(|| AppError::Aggregate("crs_wkt missing BBOX clause".into()))?;
    let after = &wkt[start + "BBOX[".len()..];
    let end = after
        .find(']')
        .ok_or_else(|| AppError::Aggregate("crs_wkt BBOX clause unterminated".into()))?;
    let body = &after[..end];

    let parts: Vec<&str> = body.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return Err(AppError::Aggregate(format!(
            "crs_wkt BBOX expected 4 values, got {}",
            parts.len()
        )));
    }
    let f = |s: &str| {
        s.parse::<f64>()
            .map_err(|e| AppError::Aggregate(format!("crs_wkt BBOX value {s:?}: {e}")))
    };
    let s = f(parts[0])?;
    let w = f(parts[1])?;
    let n = f(parts[2])?;
    let e = f(parts[3])?;
    if !(s < n && w < e) {
        return Err(AppError::Aggregate(format!(
            "crs_wkt BBOX not well-ordered: south={s} west={w} north={n} east={e}"
        )));
    }
    Ok((s, w, n, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARPEGE_WKT: &str = "GEOGCRS[\"WGS 84\",\n    DATUM[\"World Geodetic System 1984\",\n        ELLIPSOID[\"WGS 84\",6378137,298.257223563]],\n    CS[ellipsoidal,2],\n        AXIS[\"latitude\",north],\n        AXIS[\"longitude\",east],\n        ANGLEUNIT[\"degree\",0.0174532925199433]\n    USAGE[\n        SCOPE[\"grid\"],\n        BBOX[20.0,-32.0,72.0,42.0]]]";

    #[test]
    fn parses_arpege_bbox() {
        let (s, w, n, e) = parse_wkt_bbox(ARPEGE_WKT).unwrap();
        assert_eq!((s, w, n, e), (20.0, -32.0, 72.0, 42.0));
    }

    #[test]
    fn rejects_missing_bbox() {
        let err = parse_wkt_bbox("GEOGCRS[\"WGS 84\"]").unwrap_err();
        assert!(matches!(err, AppError::Aggregate(_)));
    }

    #[test]
    fn rejects_inverted_bbox() {
        let err = parse_wkt_bbox("...BBOX[72,42,20,-32]").unwrap_err();
        assert!(matches!(err, AppError::Aggregate(_)));
    }

    #[test]
    fn grid_derives_arpege_dx_dy() {
        let grid = Grid::from_wkt(ARPEGE_WKT, 521, 741).unwrap();
        // arpege_europe is documented at 0.1° native resolution.
        assert!((grid.dx() - 0.1).abs() < 1e-9, "dx={}", grid.dx());
        assert!((grid.dy() - 0.1).abs() < 1e-9, "dy={}", grid.dy());
        // Anchors: (0,0) is the SW corner, (rows-1, cols-1) is the NE corner.
        let (lat, lon) = grid.cell_lat_lon(0, 0);
        assert!((lat - 20.0).abs() < 1e-9 && (lon - (-32.0)).abs() < 1e-9);
        let (lat, lon) = grid.cell_lat_lon(520, 740);
        assert!((lat - 72.0).abs() < 1e-9 && (lon - 42.0).abs() < 1e-9);
    }

    #[test]
    fn cells_in_bbox_clips_to_grid() {
        let grid = Grid::from_wkt(ARPEGE_WKT, 521, 741).unwrap();
        // France-ish tile, well inside the grid.
        let r = grid
            .cells_in_bbox(LatLonBBox {
                south: 43.0,
                west: 0.0,
                north: 50.0,
                east: 6.0,
            })
            .unwrap();
        // Width:  (6 - 0) / 0.1 ≈ 60 cells, plus first row → 61.
        // Height: (50 - 43) / 0.1 ≈ 70 cells, plus first row → 71.
        assert_eq!(r.j_max - r.j_min + 1, 61);
        assert_eq!(r.i_max - r.i_min + 1, 71);
    }

    #[test]
    fn cells_in_bbox_returns_none_for_disjoint() {
        let grid = Grid::from_wkt(ARPEGE_WKT, 521, 741).unwrap();
        assert!(
            grid.cells_in_bbox(LatLonBBox {
                south: -20.0,
                west: -100.0,
                north: -10.0,
                east: -90.0,
            })
            .is_none()
        );
    }

    #[test]
    fn tile_to_bbox_z0_is_world() {
        let b = tile_to_bbox(0, 0, 0);
        assert!((b.west + 180.0).abs() < 1e-9);
        assert!((b.east - 180.0).abs() < 1e-9);
        assert!(b.north > 85.0 && b.north < 85.1);
        assert!(b.south < -85.0 && b.south > -85.1);
    }

    #[test]
    fn tile_to_bbox_france_tile_sane() {
        // Tile z=6 x=32 y=22 sits roughly over France.
        let b = tile_to_bbox(6, 32, 22);
        assert!(b.west >= 0.0 && b.east <= 11.5);
        assert!(b.south >= 40.0 && b.north <= 56.0);
    }

    #[test]
    fn sampling_step_clamps_to_one() {
        let r = CellRange {
            i_min: 0,
            i_max: 5,
            j_min: 0,
            j_max: 9,
        };
        assert_eq!(sampling_step(r, 100), 1);
        assert_eq!(sampling_step(r, 5), 2);
        assert_eq!(sampling_step(r, 2), 5);
    }
}
