//! Fetching raw records from the three sources.
//!
//! * `osm`: ATMs, banks with ATMs and other places with `atm=yes` from the Overpass API. The
//!   default and by far the richest source.
//! * `bankaustria`: the branch finder of UniCredit Bank Austria answers with every branch in
//!   Austria; branches with cash withdrawal inside the boundary are kept.
//! * `firecrawl`: web pages found by a search are read by Firecrawl, which extracts ATM listings
//!   as JSON. Pages rarely give coordinates, so the extracted addresses are geocoded; a record
//!   is kept only when its coordinates fall inside the boundary.

use atm_finder_core::boundary::{self, Boundary};
use atm_finder_core::firecrawl::{self, Record};
use atm_finder_core::model::{self, Atm};
use atm_finder_core::sources::bankaustria;

use crate::env::get_env;
use crate::geocoders::Geocoder;
use crate::http::{Http, HttpError};
use crate::log::log;

/// Every ATM-like object inside the boundary, straight from OpenStreetMap.
pub fn fetch_osm(http: &Http, boundary: &Boundary) -> Result<Vec<Atm>, HttpError> {
    let query = boundary::atms_query(boundary.relation_id, boundary.admin_level);
    let answer = http.overpass(&query)?;
    Ok(model::parse_overpass_atms(&answer, &boundary.name))
}

/// Bank Austria branches with cash withdrawal inside the boundary.
pub fn fetch_bankaustria(http: &Http, boundary: &Boundary) -> Result<Vec<Atm>, HttpError> {
    let answer = http.get_json(bankaustria::URL)?;
    let all_branches = bankaustria::parse_locations(&answer, &boundary.name);
    let inside = all_branches
        .into_iter()
        .filter(|branch| boundary.contains(branch.lat, branch.lon))
        .collect();
    Ok(inside)
}

/// What the firecrawl source should search for and read.
pub struct FirecrawlOptions<'a> {
    /// Search queries; empty = the default queries for the place.
    pub queries: &'a [String],
    /// Pages to read directly, in addition to the search results.
    pub urls: &'a [String],
    /// Search results per query.
    pub limit: usize,
    /// Country name passed to the geocoder.
    pub country: &'a str,
}

/// ATMs from web listings via Firecrawl. Needs `FIRECRAWL_API_KEY`.
pub fn fetch_firecrawl(
    http: &Http,
    boundary: &Boundary,
    geocoder: &dyn Geocoder,
    opts: &FirecrawlOptions,
) -> Result<Vec<Atm>, String> {
    let key = get_env("FIRECRAWL_API_KEY")
        .filter(|k| !k.is_empty())
        .ok_or("source 'firecrawl' needs the environment variable FIRECRAWL_API_KEY")?;
    let pages = firecrawl_pages(http, boundary, opts, &key)?;
    let records = firecrawl::extract(&pages, &boundary.name);

    let mut atms = Vec::new();
    let mut dropped = 0;
    for record in records {
        match locate(&record, boundary, geocoder, opts.country) {
            Some((lat, lon)) => atms.push(atm_from_record(record, lat, lon, boundary)),
            None => dropped += 1,
        }
    }
    if dropped > 0 {
        log(&format!(
            "  firecrawl: {dropped} addresses could not be geocoded or lie outside {}",
            boundary.name
        ));
    }
    Ok(atms)
}

/// Run the searches and scrapes; returns `(page url, extracted JSON)` per page.
fn firecrawl_pages(
    http: &Http,
    boundary: &Boundary,
    opts: &FirecrawlOptions,
    key: &str,
) -> Result<Vec<(String, serde_json::Value)>, String> {
    let authorization = format!("Bearer {key}");
    let headers = [("Authorization", authorization.as_str())];
    // Firecrawl runs an LLM over each page with this schema and prompt.
    let json_format = serde_json::json!({
        "type": "json",
        "schema": firecrawl::schema(),
        "prompt": firecrawl::prompt(&boundary.name),
    });
    let empty = serde_json::json!({});
    let mut pages = Vec::new();

    let default_queries = firecrawl::default_queries(&boundary.name);
    let queries = if opts.queries.is_empty() {
        &default_queries[..]
    } else {
        opts.queries
    };
    for query in queries {
        log(&format!("  firecrawl search: {query:?}"));
        let request = serde_json::json!({
            "query": query,
            "limit": opts.limit,
            "country": "AT",
            "location": format!("{},Austria", boundary.name),
            "scrapeOptions": {"formats": [json_format], "onlyMainContent": true},
        });
        let answer = http
            .post_json(
                &format!("{}/search", firecrawl::API_URL),
                &headers,
                &request,
            )
            .map_err(|e| e.to_string())?;
        // Newer API versions wrap the hits in `data.web`, older ones return `data` as a list.
        let data = answer.get("data").cloned().unwrap_or_default();
        let hits = data
            .get("web")
            .and_then(|web| web.as_array())
            .cloned()
            .or_else(|| data.as_array().cloned())
            .unwrap_or_default();
        for hit in &hits {
            let url = hit.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let extracted = hit.get("json").cloned().unwrap_or_else(|| empty.clone());
            pages.push((url.to_string(), extracted));
        }
        if let Some(credits) = answer.get("creditsUsed") {
            log(&format!("    {} pages, {credits} credits", hits.len()));
        }
    }

    for url in opts.urls {
        log(&format!("  firecrawl scrape: {url}"));
        let request = serde_json::json!({
            "url": url,
            "formats": [json_format],
            "onlyMainContent": true,
        });
        let answer = http
            .post_json(
                &format!("{}/scrape", firecrawl::API_URL),
                &headers,
                &request,
            )
            .map_err(|e| e.to_string())?;
        let extracted = answer
            .get("data")
            .and_then(|data| data.get("json"))
            .cloned()
            .unwrap_or_else(|| empty.clone());
        pages.push((url.clone(), extracted));
    }
    Ok(pages)
}

/// Coordinates for an extracted record: the page's own coordinates when they lie inside the
/// boundary, otherwise the geocoded address; `None` when neither is inside.
fn locate(
    record: &Record,
    boundary: &Boundary,
    geocoder: &dyn Geocoder,
    country: &str,
) -> Option<(f64, f64)> {
    if let (Some(lat), Some(lon)) = (record.lat, record.lon) {
        if boundary.contains(lat, lon) {
            return Some((lat, lon));
        }
    }
    let geocoded = geocoder.geocode(&record.street, &record.postcode, &record.city, country);
    let (lat, lon) = match geocoded {
        Ok(Some(position)) => position,
        Ok(None) => return None,
        Err(e) => {
            log(&format!("  geocoding {}: {e}", record.street));
            return None;
        }
    };
    if boundary.contains(lat, lon) {
        Some((lat, lon))
    } else {
        None
    }
}

fn atm_from_record(record: Record, lat: f64, lon: f64, boundary: &Boundary) -> Atm {
    Atm {
        lat,
        lon,
        name: record.name,
        operator: record.operator,
        street: record.street,
        postcode: record.postcode,
        city: record.city,
        kind: "web_listing".to_string(),
        source: "firecrawl".to_string(),
        source_id: format!("{}#{}", record.page_url, record.index),
        url: record.page_url,
        place: boundary.name.clone(),
        ..Default::default()
    }
}
