//! Geometry helpers for boundaries and ATM positions.
//!
//! Everything here works on plain `(lon, lat)` pairs in WGS84 degrees, in the order GeoJSON
//! uses. Distances come from the haversine formula, which is accurate to a fraction of a percent
//! and therefore plenty for questions like "is this ATM within 40 m of that one".
//!
//! Boundaries arrive from OpenStreetMap as *multipolygon relations*: a bag of member ways tagged
//! with the role `outer` or `inner`. The ways only form closed rings once their shared endpoints
//! are joined, which is what [`assemble_rings`] and [`build_multipolygon`] do. [`GridIndex`] is a
//! small spatial hash so that merging thousands of records does not need thousands × thousands
//! distance calculations.

use std::collections::HashMap;

/// A position as `(lon, lat)` in degrees, the order GeoJSON uses.
pub type Coord = (f64, f64);
/// A closed ring of positions. The first position is usually repeated at the end.
pub type Ring = Vec<Coord>;

/// Mean Earth radius used by the haversine formula.
const EARTH_RADIUS_M: f64 = 6_371_000.0;
/// Metres per degree of latitude. Degrees of longitude shrink with the cosine of the latitude.
const M_PER_DEG_LAT: f64 = 111_320.0;

/// Great-circle distance in metres between two positions given as latitude and longitude in degrees.
pub fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (phi1, phi2) = (lat1.to_radians(), lat2.to_radians());
    let delta_phi = phi2 - phi1;
    let delta_lambda = (lon2 - lon1).to_radians();
    let a = (delta_phi / 2.0).sin().powi(2)
        + phi1.cos() * phi2.cos() * (delta_lambda / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * a.sqrt().asin()
}

/// Whether the position `(lon, lat)` lies inside `ring`.
///
/// Classic ray casting: a ray from the position towards the east crosses the ring an odd number
/// of times when the position is inside. The ring may or may not repeat its first position at
/// the end; both forms give the same answer. A position exactly on an edge counts as inside or
/// outside depending on rounding, which does not matter for ATMs.
pub fn point_in_ring(lon: f64, lat: f64, ring: &[Coord]) -> bool {
    let mut inside = false;
    // Index of the vertex before the current one; starts at the last vertex to close the ring.
    let mut previous = ring.len().wrapping_sub(1);
    for current in 0..ring.len() {
        let (x_current, y_current) = ring[current];
        let (x_previous, y_previous) = ring[previous];
        let edge_spans_latitude = (y_current > lat) != (y_previous > lat);
        if edge_spans_latitude {
            // Longitude at which this edge crosses the latitude of the position.
            let x_crossing =
                (x_previous - x_current) * (lat - y_current) / (y_previous - y_current) + x_current;
            if lon < x_crossing {
                inside = !inside;
            }
        }
        previous = current;
    }
    inside
}

/// A set of polygons, each stored as its outer ring followed by any holes.
///
/// This is the shape of an administrative boundary: usually one polygon, but exclaves add more,
/// and an enclave of another municipality shows up as a hole.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MultiPolygon {
    pub polygons: Vec<Vec<Ring>>,
}

impl MultiPolygon {
    /// Whether the position lies inside any polygon and outside that polygon's holes.
    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        for polygon in &self.polygons {
            let outer = &polygon[0];
            let holes = &polygon[1..];
            if !point_in_ring(lon, lat, outer) {
                continue;
            }
            let in_a_hole = holes.iter().any(|hole| point_in_ring(lon, lat, hole));
            if !in_a_hole {
                return true;
            }
        }
        false
    }

    /// Bounding box of the outer rings as `(min_lon, min_lat, max_lon, max_lat)`.
    pub fn bbox(&self) -> (f64, f64, f64, f64) {
        let (mut min_lon, mut min_lat, mut max_lon, mut max_lat) =
            (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for polygon in &self.polygons {
            for &(lon, lat) in &polygon[0] {
                min_lon = min_lon.min(lon);
                min_lat = min_lat.min(lat);
                max_lon = max_lon.max(lon);
                max_lat = max_lat.max(lat);
            }
        }
        (min_lon, min_lat, max_lon, max_lat)
    }

    /// The geometry as GeoJSON: a `Polygon` when there is exactly one, else a `MultiPolygon`.
    pub fn to_geojson(&self) -> serde_json::Value {
        let mut polygons: Vec<Vec<Vec<[f64; 2]>>> = Vec::new();
        for polygon in &self.polygons {
            let mut rings = Vec::new();
            for ring in polygon {
                let positions: Vec<[f64; 2]> = ring.iter().map(|&(lon, lat)| [lon, lat]).collect();
                rings.push(positions);
            }
            polygons.push(rings);
        }
        if polygons.len() == 1 {
            serde_json::json!({"type": "Polygon", "coordinates": polygons[0]})
        } else {
            serde_json::json!({"type": "MultiPolygon", "coordinates": polygons})
        }
    }
}

/// Hash-map key for a position. `f64` is not hashable, but two copies of the same OSM node
/// have bit-identical coordinates, so the bit patterns work as an exact key.
fn position_key(position: Coord) -> (u64, u64) {
    (position.0.to_bits(), position.1.to_bits())
}

/// The first segment that touches `position` and has not been used yet, in input order.
fn unused_segment_at(
    segments_at: &HashMap<(u64, u64), Vec<usize>>,
    is_unused: &[bool],
    position: Coord,
) -> Option<usize> {
    let candidates = segments_at.get(&position_key(position))?;
    candidates.iter().copied().find(|&index| is_unused[index])
}

/// Join way segments that share endpoints into closed rings.
///
/// OSM stores a boundary as many short ways. Starting from each unused way, the ring is extended
/// at its tail by any unused way that starts or ends there (walked backwards when needed) until
/// it returns to its first position. A chain that cannot be closed, which happens with incomplete
/// data, is closed artificially so the boundary still works for point-in-polygon tests.
pub fn assemble_rings(ways: Vec<Vec<Coord>>) -> Vec<Ring> {
    // A way with fewer than two positions cannot be part of a ring.
    let segments: Vec<Vec<Coord>> = ways.into_iter().filter(|way| way.len() >= 2).collect();

    // Which segments start or end at a position, so the next piece is found without scanning.
    let mut segments_at: HashMap<(u64, u64), Vec<usize>> = HashMap::new();
    for (index, segment) in segments.iter().enumerate() {
        let first = segment[0];
        let last = segment[segment.len() - 1];
        segments_at
            .entry(position_key(first))
            .or_default()
            .push(index);
        segments_at
            .entry(position_key(last))
            .or_default()
            .push(index);
    }

    let mut is_unused = vec![true; segments.len()];
    let mut rings = Vec::new();
    for start in 0..segments.len() {
        if !is_unused[start] {
            continue;
        }
        is_unused[start] = false;
        let mut ring = segments[start].clone();
        while ring[0] != ring[ring.len() - 1] {
            let tail = ring[ring.len() - 1];
            let Some(next_index) = unused_segment_at(&segments_at, &is_unused, tail) else {
                // Nothing continues here: close the ring artificially (see above).
                ring.push(ring[0]);
                break;
            };
            is_unused[next_index] = false;
            let segment = &segments[next_index];
            if segment[0] == tail {
                ring.extend_from_slice(&segment[1..]);
            } else {
                // The way points the other way round: walk it backwards.
                ring.extend(segment.iter().rev().skip(1).copied());
            }
        }
        rings.push(ring);
    }
    rings
}

/// Assemble the member ways of an OSM multipolygon relation into polygons with holes.
///
/// `outer` and `inner` are the ways by their member role. Each inner ring becomes a hole of the
/// first outer polygon that contains its first position; an inner ring that lies in no outer
/// polygon (broken data) is dropped.
pub fn build_multipolygon(outer: Vec<Vec<Coord>>, inner: Vec<Vec<Coord>>) -> MultiPolygon {
    let mut polygons: Vec<Vec<Ring>> = Vec::new();
    for outer_ring in assemble_rings(outer) {
        polygons.push(vec![outer_ring]);
    }
    for hole in assemble_rings(inner) {
        let (probe_lon, probe_lat) = hole[0];
        let containing_polygon = polygons
            .iter_mut()
            .find(|polygon| point_in_ring(probe_lon, probe_lat, &polygon[0]));
        if let Some(polygon) = containing_polygon {
            polygon.push(hole);
        }
    }
    MultiPolygon { polygons }
}

/// A spatial hash: item indices bucketed into cells of roughly `cell_m` metres.
///
/// To find everything within some distance of a position, look at the position's cell and its
/// eight neighbours. Callers choose a cell size of at least the search distance so that no
/// neighbour is missed. Cells are measured in degrees; the longitude cell width is derived from a
/// reference latitude, which is accurate enough across Austria.
pub struct GridIndex {
    cell_lat: f64,
    cell_lon: f64,
    cells: HashMap<(i64, i64), Vec<usize>>,
}

impl GridIndex {
    /// An empty index with cells of `cell_m` metres, sized for positions around `ref_lat` degrees.
    pub fn new(cell_m: f64, ref_lat: f64) -> Self {
        let metres_per_deg_lon = M_PER_DEG_LAT * ref_lat.to_radians().cos().max(0.1);
        GridIndex {
            cell_lat: cell_m / M_PER_DEG_LAT,
            cell_lon: cell_m / metres_per_deg_lon,
            cells: HashMap::new(),
        }
    }

    fn cell_of(&self, lat: f64, lon: f64) -> (i64, i64) {
        let row = (lat / self.cell_lat).floor() as i64;
        let column = (lon / self.cell_lon).floor() as i64;
        (row, column)
    }

    /// Store `item` (an index into the caller's list) at the given position.
    pub fn add(&mut self, lat: f64, lon: f64, item: usize) {
        self.cells
            .entry(self.cell_of(lat, lon))
            .or_default()
            .push(item);
    }

    /// Items in the cell of the position and the eight surrounding cells, in insertion order per cell.
    pub fn near(&self, lat: f64, lon: f64) -> Vec<usize> {
        let (row, column) = self.cell_of(lat, lon);
        let mut items = Vec::new();
        for row_offset in -1..=1 {
            for column_offset in -1..=1 {
                if let Some(cell_items) =
                    self.cells.get(&(row + row_offset, column + column_offset))
                {
                    items.extend_from_slice(cell_items);
                }
            }
        }
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: f64, y0: f64, size: f64) -> Ring {
        vec![
            (x0, y0),
            (x0 + size, y0),
            (x0 + size, y0 + size),
            (x0, y0 + size),
            (x0, y0),
        ]
    }

    #[test]
    fn haversine_graz_hauptplatz_to_hauptbahnhof() {
        let d = haversine_m(47.0707, 15.4395, 47.0727, 15.4166);
        assert!((d - 1750.0).abs() < 100.0, "{d}");
    }

    #[test]
    fn point_in_ring_with_and_without_closing_point() {
        let closed = square(0.0, 0.0, 10.0);
        let open = &closed[..4];
        assert!(point_in_ring(5.0, 5.0, &closed) && point_in_ring(5.0, 5.0, open));
        assert!(!point_in_ring(15.0, 5.0, &closed));
    }

    #[test]
    fn multipolygon_with_hole() {
        let mp = MultiPolygon {
            polygons: vec![vec![square(0.0, 0.0, 10.0), square(4.0, 4.0, 2.0)]],
        };
        assert!(mp.contains(1.0, 1.0));
        assert!(!mp.contains(5.0, 5.0));
        assert_eq!(mp.bbox(), (0.0, 0.0, 10.0, 10.0));
        assert_eq!(mp.to_geojson()["type"], "Polygon");
    }

    #[test]
    fn assemble_rings_joins_and_reverses_ways() {
        let ways = vec![
            vec![(0.0, 0.0), (10.0, 0.0)],
            vec![(10.0, 10.0), (10.0, 0.0)], // reversed
            vec![(10.0, 10.0), (0.0, 10.0), (0.0, 0.0)],
        ];
        let rings = assemble_rings(ways);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].len(), 5);
        assert_eq!(rings[0][0], rings[0][4]);
    }

    #[test]
    fn open_chain_is_closed_and_hole_assigned() {
        let mp = build_multipolygon(
            vec![vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]],
            vec![vec![
                (4.0, 4.0),
                (6.0, 4.0),
                (6.0, 6.0),
                (4.0, 6.0),
                (4.0, 4.0),
            ]],
        );
        assert_eq!(mp.polygons.len(), 1);
        assert_eq!(mp.polygons[0].len(), 2);
        assert!(mp.contains(1.0, 1.0) && !mp.contains(5.0, 5.0));
    }

    #[test]
    fn grid_index_finds_neighbours_across_cells() {
        let mut g = GridIndex::new(80.0, 47.0);
        g.add(47.0, 15.0, 0);
        g.add(47.0 + 30.0 / M_PER_DEG_LAT, 15.0, 1);
        g.add(47.1, 15.0, 2);
        let near = g.near(47.0 + 15.0 / M_PER_DEG_LAT, 15.0);
        assert!(near.contains(&0) && near.contains(&1) && !near.contains(&2));
    }
}
