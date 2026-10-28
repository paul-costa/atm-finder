//! Completing the addresses of ATMs that OpenStreetMap lists without one (`--fill-addresses`).
//!
//! Roughly half of the ATMs in OSM carry no `addr:*` tags. Two steps fill the gap:
//!
//! 1. The nearest OSM object with a house number within 50 metres, fetched in batches of 150
//!    ATMs per Overpass query. This gives real house numbers and, measured for Graz, lands
//!    within 50 m of Google's position in about four out of five cases.
//! 2. For the rest, reverse geocoding (Nominatim or Google), which often returns only a street
//!    name. Nominatim allows one request per second, so this step is slow for large areas.

use atm_finder_core::addresses::{match_nearest, nearby_query, Hit, BATCH_SIZE};
use atm_finder_core::model::Atm;
use atm_finder_core::ADDRESS_RADIUS_M;

use crate::geocoders::Geocoder;
use crate::http::Http;
use crate::log::log;

/// How many addresses were completed, and how.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FillCounts {
    /// ATMs without a street before the run.
    pub missing: usize,
    /// Completed from a nearby OSM house number.
    pub nearby: usize,
    /// Completed by reverse geocoding.
    pub reverse: usize,
    /// Still without an address.
    pub unresolved: usize,
}

/// Fill `street`, `postcode` and `city` of every ATM that has no street yet.
pub fn fill_addresses(http: &Http, geocoder: &dyn Geocoder, atms: &mut [Atm]) -> FillCounts {
    let missing: Vec<usize> = (0..atms.len())
        .filter(|&i| atms[i].street.is_empty())
        .collect();
    let mut counts = FillCounts {
        missing: missing.len(),
        ..Default::default()
    };
    if missing.is_empty() {
        return counts;
    }

    log(&format!(
        "Filling addresses for {} records: nearest OSM house number (<= {:.0} m) first ...",
        missing.len(),
        ADDRESS_RADIUS_M
    ));
    let mut still_missing = Vec::new();
    for batch in missing.chunks(BATCH_SIZE) {
        let hits = nearby_house_numbers(http, atms, batch);
        for (&index, hit) in batch.iter().zip(hits) {
            match hit {
                Some(hit) => {
                    apply_nearby_hit(&mut atms[index], hit);
                    counts.nearby += 1;
                }
                None => still_missing.push(index),
            }
        }
    }

    if !still_missing.is_empty() {
        log(&format!(
            "  {} without an OSM house number nearby, reverse geocoding ({}) ...",
            still_missing.len(),
            geocoder.name()
        ));
    }
    for index in still_missing {
        let atm = &mut atms[index];
        match geocoder.reverse(atm.lat, atm.lon) {
            Ok(Some(address)) => {
                apply_reverse_hit(atm, address, geocoder.name());
                counts.reverse += 1;
            }
            Ok(None) => counts.unresolved += 1,
            Err(e) => {
                log(&format!("  {}: {e}", atm.stable_id()));
                counts.unresolved += 1;
            }
        }
    }
    counts
}

/// The nearest house number for each ATM of one batch (`None` where there is none within
/// range, or for the whole batch when Overpass fails; the caller then reverse-geocodes them).
fn nearby_house_numbers(http: &Http, atms: &[Atm], batch: &[usize]) -> Vec<Option<Hit>> {
    let points: Vec<(f64, f64)> = batch.iter().map(|&i| (atms[i].lat, atms[i].lon)).collect();
    match http.overpass(&nearby_query(&points, ADDRESS_RADIUS_M)) {
        Ok(answer) => match_nearest(&points, &answer, ADDRESS_RADIUS_M),
        Err(e) => {
            log(&format!("  OSM address lookup failed: {e}"));
            vec![None; points.len()]
        }
    }
}

/// Take street, and postcode/city where still empty, from a nearby house number.
fn apply_nearby_hit(atm: &mut Atm, hit: Hit) {
    atm.street = hit.street;
    if atm.postcode.is_empty() {
        atm.postcode = hit.postcode;
    }
    if atm.city.is_empty() {
        atm.city = hit.city;
    }
    atm.address_source = "osm_nearby".to_string();
}

/// Take the reverse-geocoded address; the `address_source` column names the geocoder.
fn apply_reverse_hit(atm: &mut Atm, address: (String, String, String), geocoder_name: &str) {
    let (street, postcode, city) = address;
    atm.street = street;
    if atm.postcode.is_empty() {
        atm.postcode = postcode;
    }
    if atm.city.is_empty() {
        atm.city = city;
    }
    atm.address_source = format!("{geocoder_name}_reverse");
}
