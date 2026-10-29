//! The network side of the website (browser only).
//!
//! Everything that needs an HTTP request lives here: looking up boundaries and ATMs on the
//! Overpass API, completing addresses, and reading the list of ready-made files. The browser's
//! `fetch` is used through `gloo-net`; the Overpass and Nominatim servers allow cross-origin
//! requests, which is what makes a server-less site possible.
//!
//! Status messages are reported through a callback that receives an i18n key, so this module
//! stays free of UI text.

use std::time::Duration;

use gloo_net::http::Request;
use gloo_timers::future::sleep;

use atm_finder_core::addresses::{self, BATCH_SIZE};
use atm_finder_core::boundary::{self, Boundary};
use atm_finder_core::model::{self, Atm};
use atm_finder_core::ADDRESS_RADIUS_M;

/// Overpass instances in the order they are tried: the main one, then a public mirror for the
/// times when the main instance answers "at capacity".
pub const OVERPASS_ENDPOINTS: [&str; 2] = [
    "https://overpass-api.de/api/interpreter",
    "https://overpass.kumi.systems/api/interpreter",
];
/// Nominatim, used for reverse geocoding only; its policy allows one request per second.
pub const NOMINATIM_URL: &str = "https://nominatim.openstreetmap.org";

/// Callback for status messages; receives an i18n key such as `status_retry`.
pub type StatusFn<'a> = &'a dyn Fn(&str);

/// Error pages from Overpass are HTML; keep the readable text, at most `limit` characters.
fn strip_html(text: &str, limit: usize) -> String {
    let mut plain = String::new();
    let mut inside_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if !inside_tag => plain.push(ch),
            _ => {}
        }
    }
    let single_spaced = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    single_spaced.chars().take(limit).collect()
}

/// POST a query to an Overpass endpoint as a form, returning status code and body.
async fn post_overpass(url: &str, query: &str) -> Result<(u16, String), String> {
    let encoded_query: String = js_sys::encode_uri_component(query).into();
    let body = format!("data={encoded_query}");
    let request = Request::post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .map_err(|e| e.to_string())?;
    let response = request.send().await.map_err(|e| e.to_string())?;
    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    Ok((status, text))
}

/// Run an Overpass QL query and parse the JSON answer.
///
/// Each endpoint gets two attempts (the second after a short pause) before the next mirror is
/// tried; only overload and server errors (429, 5xx) and network failures move on, a 400 means
/// the query is wrong and is reported at once.
pub async fn overpass_json(query: &str, status: StatusFn<'_>) -> Result<serde_json::Value, String> {
    let mut last_error = String::from("no Overpass endpoint");
    for (endpoint_index, url) in OVERPASS_ENDPOINTS.iter().enumerate() {
        if endpoint_index > 0 {
            status("status_mirror");
        }
        for attempt in 0..2u64 {
            if attempt > 0 {
                status("status_retry");
                sleep(Duration::from_secs(5 * attempt)).await;
            }
            match post_overpass(url, query).await {
                Ok((200, text)) => {
                    return serde_json::from_str(&text)
                        .map_err(|e| format!("invalid JSON from Overpass: {e}"));
                }
                Ok((code, text)) if code == 429 || code >= 500 => {
                    last_error = format!("HTTP {code}: {}", strip_html(&text, 200));
                }
                Ok((code, text)) => return Err(format!("HTTP {code}: {}", strip_html(&text, 300))),
                Err(network_error) => last_error = network_error,
            }
        }
    }
    Err(last_error)
}

/// Boundaries whose name contains `name`, at the given admin levels, without geometry.
pub async fn find_candidates(
    name: &str,
    levels: &[u8],
    status: StatusFn<'_>,
) -> Result<Vec<Boundary>, String> {
    let query = boundary::candidates_query(name, levels, false);
    let data = overpass_json(&query, status).await?;
    Ok(boundary::parse_candidates(&data))
}

/// Load the polygon of a boundary, and its tags, name and level when they are still unknown
/// (a boundary given by relation id has only the id).
pub async fn load_geometry(boundary: &mut Boundary, status: StatusFn<'_>) -> Result<(), String> {
    let data = overpass_json(&boundary::geometry_query(boundary.relation_id), status).await?;
    let (tags, geometry) = boundary::parse_geometry(&data, boundary.relation_id)?;
    if boundary.tags.is_empty() {
        boundary.tags = tags;
    }
    if boundary.admin_level == 0 {
        let level_tag = boundary.tags.get("admin_level");
        boundary.admin_level = level_tag
            .and_then(|level| level.parse::<u8>().ok())
            .unwrap_or(0);
    }
    if boundary.name.is_empty() {
        boundary.name = match boundary.tags.get("name") {
            Some(name) => name.clone(),
            None => format!("relation {}", boundary.relation_id),
        };
    }
    boundary.geometry = Some(geometry);
    Ok(())
}

/// The raw ATM records inside a boundary, not yet merged.
pub async fn fetch_atms(boundary: &Boundary, status: StatusFn<'_>) -> Result<Vec<Atm>, String> {
    let query = boundary::atms_query(boundary.relation_id, boundary.admin_level);
    let data = overpass_json(&query, status).await?;
    Ok(model::parse_overpass_atms(&data, &boundary.name))
}

/// How the address completion went.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FillCounts {
    /// ATMs without an address before the run.
    pub missing: usize,
    /// Completed from a house number nearby.
    pub nearby: usize,
    /// Completed by reverse geocoding.
    pub reverse: usize,
    /// Still without an address.
    pub unresolved: usize,
}

/// Store a found address on an ATM; postcode and city only fill gaps, the street always wins.
fn apply_address(atm: &mut Atm, street: String, postcode: String, city: String, source: &str) {
    atm.street = street;
    if atm.postcode.is_empty() {
        atm.postcode = postcode;
    }
    if atm.city.is_empty() {
        atm.city = city;
    }
    atm.address_source = source.to_string();
}

/// One Nominatim reverse lookup; the caller keeps to one request per second.
async fn nominatim_reverse(lat: f64, lon: f64) -> Result<serde_json::Value, String> {
    let url = format!("{NOMINATIM_URL}/reverse?lat={lat:.7}&lon={lon:.7}&format=jsonv2&zoom=18");
    let response = Request::get(&url).send().await.map_err(|e| e.to_string())?;
    if response.status() != 200 {
        return Err(format!("Nominatim HTTP {}", response.status()));
    }
    let text = response.text().await.map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// Complete street, postcode and city for the ATMs without a street.
///
/// First the nearest house number from OpenStreetMap, one Overpass query per batch of ATMs;
/// then, for whatever is left, Nominatim reverse geocoding at one request per second.
/// `progress` is told how many of the missing addresses are done so far.
pub async fn fill_addresses(
    rows: &mut [Atm],
    status: StatusFn<'_>,
    progress: &dyn Fn(usize, usize),
) -> FillCounts {
    let missing: Vec<usize> = (0..rows.len())
        .filter(|&index| rows[index].street.is_empty())
        .collect();
    let mut counts = FillCounts {
        missing: missing.len(),
        ..Default::default()
    };
    if missing.is_empty() {
        return counts;
    }

    status("status_addresses");
    let mut still_missing: Vec<usize> = Vec::new();
    for batch in missing.chunks(BATCH_SIZE) {
        let points: Vec<(f64, f64)> = batch
            .iter()
            .map(|&index| (rows[index].lat, rows[index].lon))
            .collect();
        let query = addresses::nearby_query(&points, ADDRESS_RADIUS_M);
        let hits = match overpass_json(&query, status).await {
            Ok(data) => addresses::match_nearest(&points, &data, ADDRESS_RADIUS_M),
            // A failed batch is not fatal: its ATMs go to reverse geocoding instead.
            Err(_) => vec![None; points.len()],
        };
        for (&index, hit) in batch.iter().zip(hits) {
            match hit {
                Some(hit) => {
                    apply_address(
                        &mut rows[index],
                        hit.street,
                        hit.postcode,
                        hit.city,
                        "osm_nearby",
                    );
                    counts.nearby += 1;
                }
                None => still_missing.push(index),
            }
        }
        progress(counts.nearby, missing.len());
    }

    if !still_missing.is_empty() {
        status("status_reverse");
    }
    let reverse_total = still_missing.len();
    for (done, index) in still_missing.into_iter().enumerate() {
        if done > 0 {
            sleep(Duration::from_millis(1100)).await;
        }
        let (lat, lon) = (rows[index].lat, rows[index].lon);
        let found = match nominatim_reverse(lat, lon).await {
            Ok(data) => addresses::parse_nominatim_reverse(&data),
            Err(_) => None,
        };
        match found {
            Some((street, postcode, city)) => {
                apply_address(
                    &mut rows[index],
                    street,
                    postcode,
                    city,
                    "nominatim_reverse",
                );
                counts.reverse += 1;
            }
            None => counts.unresolved += 1,
        }
        progress(counts.nearby + done + 1, counts.nearby + reverse_total);
    }
    counts
}

/// One entry of `data/index.json`, the list of ready-made exports next to the page.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct ReadyEntry {
    /// File name without extension, e.g. `graz`.
    pub slug: String,
    /// Place name shown to the user.
    pub name: String,
    /// Admin level key when the export was made for one, else empty.
    #[serde(default)]
    pub level: String,
    /// Number of ATMs in the file.
    pub rows: usize,
    /// Export date as `YYYY-MM-DD`.
    #[serde(default)]
    pub updated: String,
}

/// The list of ready-made exports shipped next to the page.
pub async fn load_ready_index() -> Result<Vec<ReadyEntry>, String> {
    let response = Request::get("data/index.json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if response.status() != 200 {
        return Err(format!("HTTP {}", response.status()));
    }
    let text = response.text().await.map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- Google Maps lists (server API)

/// A public Google Maps list the server created for a place (`GET api/lists`).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ListEntry {
    /// Same slug as the ready-made files, e.g. `graz` or `graz-umgebung-district`.
    pub slug: String,
    /// OSM relation id of the boundary.
    pub relation: u64,
    /// The public share link of the list.
    pub url: String,
    /// ATMs that went into the list (Google knows them as places).
    pub in_list: usize,
    /// All ATMs of the place.
    pub total: usize,
}

/// The creation lock: one list at a time, then a cooldown for everyone.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ListLock {
    /// `free`, `running` or `cooldown`.
    pub state: String,
    /// Place being created (while `running`).
    #[serde(default)]
    pub place: String,
    /// OSM relation id being created (while `running`).
    #[serde(default)]
    pub relation: u64,
    /// When creating is possible again, as local `HH:MM` (empty when `free`).
    #[serde(default)]
    pub until: String,
    /// Message of the last failed run for `relation`, if any.
    #[serde(default)]
    pub error: String,
}

/// Why this visitor may not create a list now (only in 429/403 answers).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ListQuota {
    /// `hour`, `day` or `blocked`.
    pub reason: String,
    /// The number of lists that was reached (per hour, per day, or in total).
    #[serde(default)]
    pub limit: u32,
    /// When this visitor can create the next list (`HH:MM` or `DD.MM. HH:MM`; empty when blocked).
    #[serde(default)]
    pub until: String,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ListsState {
    pub lists: Vec<ListEntry>,
    pub lock: ListLock,
    /// Set when the server refused this visitor (per-visitor limit).
    #[serde(default)]
    pub quota: Option<ListQuota>,
}

/// The lists and the lock state; fails when the site runs without the server (e.g. local tests).
pub async fn load_lists() -> Result<ListsState, String> {
    let response = Request::get("api/lists")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if response.status() != 200 {
        return Err(format!("HTTP {}", response.status()));
    }
    let text = response.text().await.map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// Ask the server to create the list for a boundary. The answer is the new state either way:
/// 202 = started, 409 = locked, 200 = the list already exists, 429/403 = this visitor's limit
/// (then `quota` says why).
pub async fn create_list(relation: u64, name: &str) -> Result<ListsState, String> {
    let body = serde_json::json!({ "relation": relation, "name": name }).to_string();
    let response = Request::post("api/lists")
        .header("Content-Type", "application/json")
        .body(body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let text = response.text().await.map_err(|e| e.to_string())?;
    match response.status() {
        200 | 202 | 403 | 409 | 429 => serde_json::from_str(&text).map_err(|e| e.to_string()),
        status => Err(format!("HTTP {status}")),
    }
}
