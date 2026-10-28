//! Turning addresses into coordinates and back.
//!
//! Two geocoders are available. **Nominatim** (OpenStreetMap) is free and needs no key; its
//! usage policy allows at most one request per second and asks for a contact e-mail, which
//! this module honours by pausing between requests. **Google** needs an API key
//! (`GOOGLE_MAPS_API_KEY`) and allows parallel requests, but its terms permit storing results
//! for 30 days only and forbid showing them on non-Google maps, so it is a tool for one-off
//! lists, not for data that is published.
//!
//! Both cache their answers for the duration of the run, so identical addresses cost one
//! request.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

use atm_finder_core::addresses::parse_nominatim_reverse;

use crate::env::get_env;
use crate::http::{form_encode, Http, HttpError};

const GOOGLE_GEOCODE_URL: &str = "https://maps.googleapis.com/maps/api/geocode/json";

/// The minimum gap between two Nominatim requests (the policy says one per second).
const NOMINATIM_MIN_GAP: Duration = Duration::from_millis(1100);

/// One geocoding answer for a free-text address.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub lat: f64,
    pub lon: f64,
    /// The address as the geocoder understood it.
    pub address: String,
    /// How precise the match is: Google's `ROOFTOP`, `RANGE_INTERPOLATED`, ...; for Nominatim
    /// the type of the matched OSM object (`building`, `amenity`, `place`, ...).
    pub accuracy: String,
    /// `yes` when Google could only match part of the address (worth checking by hand).
    pub partial_match: String,
}

/// A structured address: (street with house number, postcode, city).
pub type Address = (String, String, String);

/// What the tool needs from a geocoder.
pub trait Geocoder: Send + Sync {
    /// `nominatim` or `google`; also used in the `geo_source` and `address_source` columns.
    fn name(&self) -> &'static str;
    /// Whether several lookups may run at the same time.
    fn parallel(&self) -> bool;
    /// How many requests were sent so far (for the cost summary).
    fn requests(&self) -> usize;
    /// Coordinates and details for a free-text address, `None` when nothing matches.
    fn lookup(&self, address: &str) -> Result<Option<Hit>, HttpError>;
    /// Coordinates for a structured address, `None` when nothing matches.
    fn geocode(
        &self,
        street: &str,
        postcode: &str,
        city: &str,
        country: &str,
    ) -> Result<Option<(f64, f64)>, HttpError>;
    /// The nearest address for a coordinate, `None` when there is no street nearby.
    fn reverse(&self, lat: f64, lon: f64) -> Result<Option<Address>, HttpError>;
}

// ------------------------------------------------------------------ Nominatim

pub struct Nominatim {
    http: Arc<Http>,
    /// ISO code the free-text lookup is restricted to (empty = worldwide).
    country_code: String,
    /// Answers of this run, by lookup key.
    cache: Mutex<HashMap<String, Option<Hit>>>,
    /// When the previous request was sent, to keep the one-per-second limit.
    last_request: Mutex<Option<Instant>>,
    requests: AtomicUsize,
}

impl Nominatim {
    pub fn new(http: Arc<Http>, country_code: &str) -> Self {
        Nominatim {
            http,
            country_code: country_code.to_lowercase(),
            cache: Mutex::new(HashMap::new()),
            last_request: Mutex::new(None),
            requests: AtomicUsize::new(0),
        }
    }

    /// Send one request, waiting first if the previous one was less than a second ago.
    fn throttled(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<serde_json::Value, HttpError> {
        let mut last_request = self.last_request.lock().unwrap();
        if let Some(previous) = *last_request {
            let since_previous = previous.elapsed();
            if since_previous < NOMINATIM_MIN_GAP {
                sleep(NOMINATIM_MIN_GAP - since_previous);
            }
        }
        self.requests.fetch_add(1, Ordering::Relaxed);
        let answer = self.http.nominatim(path, params);
        *last_request = Some(Instant::now());
        answer
    }

    /// The first result of a Nominatim `search` answer as a [`Hit`].
    fn hit_from_search_results(results: &serde_json::Value) -> Option<Hit> {
        let first = results.as_array()?.first()?;
        let number = |key: &str| {
            first
                .get(key)
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
        };
        let text = |key: &str| first.get(key).and_then(|v| v.as_str()).unwrap_or("");
        let object_type = first
            .get("addresstype")
            .or_else(|| first.get("type"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        Some(Hit {
            lat: number("lat")?,
            lon: number("lon")?,
            address: text("display_name").to_string(),
            accuracy: object_type.to_string(),
            partial_match: String::new(),
        })
    }

    fn cached(&self, key: &str) -> Option<Option<Hit>> {
        self.cache.lock().unwrap().get(key).cloned()
    }

    fn remember(&self, key: String, hit: &Option<Hit>) {
        self.cache.lock().unwrap().insert(key, hit.clone());
    }
}

impl Geocoder for Nominatim {
    fn name(&self) -> &'static str {
        "nominatim"
    }

    fn parallel(&self) -> bool {
        false
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }

    fn lookup(&self, address: &str) -> Result<Option<Hit>, HttpError> {
        let key = format!("lookup|{address}");
        if let Some(hit) = self.cached(&key) {
            return Ok(hit);
        }
        let mut params = vec![
            ("q", address.to_string()),
            ("format", "jsonv2".to_string()),
            ("limit", "1".to_string()),
        ];
        if !self.country_code.is_empty() {
            params.push(("countrycodes", self.country_code.clone()));
        }
        let hit = Self::hit_from_search_results(&self.throttled("search", &params)?);
        self.remember(key, &hit);
        Ok(hit)
    }

    fn geocode(
        &self,
        street: &str,
        postcode: &str,
        city: &str,
        country: &str,
    ) -> Result<Option<(f64, f64)>, HttpError> {
        let key = format!(
            "geocode|{}|{}|{}|{}",
            street.trim().to_lowercase(),
            postcode.trim(),
            city.trim().to_lowercase(),
            country.trim().to_lowercase()
        );
        if let Some(hit) = self.cached(&key) {
            return Ok(hit.map(|h| (h.lat, h.lon)));
        }
        let params = [
            ("street", street.to_string()),
            ("postalcode", postcode.to_string()),
            ("city", city.to_string()),
            ("country", country.to_string()),
            ("format", "jsonv2".to_string()),
            ("limit", "1".to_string()),
        ];
        let hit = Self::hit_from_search_results(&self.throttled("search", &params)?);
        self.remember(key, &hit);
        Ok(hit.map(|h| (h.lat, h.lon)))
    }

    fn reverse(&self, lat: f64, lon: f64) -> Result<Option<Address>, HttpError> {
        // zoom 18 asks for building-level detail, i.e. a house number when OSM has one.
        let params = [
            ("lat", format!("{lat:.7}")),
            ("lon", format!("{lon:.7}")),
            ("format", "jsonv2".to_string()),
            ("zoom", "18".to_string()),
        ];
        let answer = self.throttled("reverse", &params)?;
        Ok(parse_nominatim_reverse(&answer))
    }
}

// ------------------------------------------------------------------ Google

/// Google Geocoding API (v3). The key comes from `GOOGLE_MAPS_API_KEY` (environment or `.env`).
pub struct Google {
    http: Arc<Http>,
    key: String,
    /// ISO code used as a component filter and region bias (empty = worldwide).
    country_code: String,
    /// Language of the returned addresses.
    language: String,
    /// Answers of this run, by free-text address.
    cache: Mutex<HashMap<String, Option<Hit>>>,
    requests: AtomicUsize,
}

impl Google {
    /// Fails with a readable message when no plausible key is configured, so a typo in the key
    /// is reported before any address is sent.
    pub fn new(http: Arc<Http>, country_code: &str) -> Result<Self, String> {
        let key = read_google_key()?;
        Ok(Google {
            http,
            key,
            country_code: country_code.to_lowercase(),
            language: "de".to_string(),
            cache: Mutex::new(HashMap::new()),
            requests: AtomicUsize::new(0),
        })
    }

    /// Send one geocoding request and return its `results` array.
    ///
    /// `OVER_QUERY_LIMIT` (too many requests per second) is retried a few times with a pause.
    /// `REQUEST_DENIED` and `OVER_DAILY_LIMIT` mean the key or the billing is broken: the error
    /// is marked fatal so callers stop instead of failing once per address.
    fn request_results(
        &self,
        params: &[(&str, String)],
    ) -> Result<Vec<serde_json::Value>, HttpError> {
        let mut query: Vec<String> = params
            .iter()
            .map(|(name, value)| format!("{name}={}", form_encode(value)))
            .collect();
        query.push(format!("key={}", form_encode(&self.key)));
        query.push(format!("language={}", self.language));
        let url = format!("{GOOGLE_GEOCODE_URL}?{}", query.join("&"));

        const RATE_LIMIT_RETRIES: u64 = 3;
        for attempt in 0..=RATE_LIMIT_RETRIES {
            self.requests.fetch_add(1, Ordering::Relaxed);
            let answer = self.http.get_json(&url)?;
            let status = answer.get("status").and_then(|v| v.as_str()).unwrap_or("");
            match status {
                "OK" => {
                    let results = answer.get("results").and_then(|r| r.as_array());
                    return Ok(results.cloned().unwrap_or_default());
                }
                "ZERO_RESULTS" => return Ok(Vec::new()),
                "OVER_QUERY_LIMIT" if attempt < RATE_LIMIT_RETRIES => {
                    sleep(Duration::from_secs(2 * (attempt + 1)));
                }
                _ => {
                    let detail = answer
                        .get("error_message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let message = format!("Google Geocoding: {status} {detail}")
                        .trim()
                        .to_string();
                    let mut error = HttpError::new(message);
                    error.fatal = matches!(status, "REQUEST_DENIED" | "OVER_DAILY_LIMIT");
                    return Err(error);
                }
            }
        }
        Ok(Vec::new())
    }

    /// One Google result as a [`Hit`]; `None` when it lacks coordinates.
    fn hit_from_result(result: &serde_json::Value) -> Option<Hit> {
        let geometry = result.get("geometry")?;
        let location = geometry.get("location")?;
        let partial = result
            .get("partial_match")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Some(Hit {
            lat: location.get("lat")?.as_f64()?,
            lon: location.get("lng")?.as_f64()?,
            address: text(result, "formatted_address"),
            accuracy: text(geometry, "location_type"),
            partial_match: if partial {
                "yes".to_string()
            } else {
                String::new()
            },
        })
    }

    /// Google's `address_components` as a map from component type to long name.
    fn address_components_by_type(result: &serde_json::Value) -> HashMap<String, String> {
        let mut by_type = HashMap::new();
        let components = result.get("address_components").and_then(|v| v.as_array());
        for component in components.into_iter().flatten() {
            let name = text(component, "long_name");
            let types = component.get("types").and_then(|v| v.as_array());
            for component_type in types.into_iter().flatten() {
                if let Some(component_type) = component_type.as_str() {
                    by_type.insert(component_type.to_string(), name.clone());
                }
            }
        }
        by_type
    }
}

/// The configured Google key, or a message explaining what is missing.
fn read_google_key() -> Result<String, String> {
    let key = get_env("GOOGLE_MAPS_API_KEY")
        .unwrap_or_default()
        .trim()
        .to_string();
    if key.is_empty() {
        return Err(
            "No Google key found: set GOOGLE_MAPS_API_KEY (environment or .env file).".to_string(),
        );
    }
    let looks_like_placeholder = key.contains("...") || key.len() < 30;
    if looks_like_placeholder {
        return Err("GOOGLE_MAPS_API_KEY looks like a placeholder; paste the real key from the Google Cloud Console.".to_string());
    }
    Ok(key)
}

/// A string field of a JSON object, empty when missing.
fn text(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

impl Geocoder for Google {
    fn name(&self) -> &'static str {
        "google"
    }

    fn parallel(&self) -> bool {
        true
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }

    fn lookup(&self, address: &str) -> Result<Option<Hit>, HttpError> {
        if let Some(hit) = self.cache.lock().unwrap().get(address) {
            return Ok(hit.clone());
        }
        let mut params = vec![("address", address.to_string())];
        if !self.country_code.is_empty() {
            params.push(("components", format!("country:{}", self.country_code)));
            params.push(("region", self.country_code.clone()));
        }
        let results = self.request_results(&params)?;
        let hit = results.first().and_then(Self::hit_from_result);
        self.cache
            .lock()
            .unwrap()
            .insert(address.to_string(), hit.clone());
        Ok(hit)
    }

    fn geocode(
        &self,
        street: &str,
        postcode: &str,
        city: &str,
        country: &str,
    ) -> Result<Option<(f64, f64)>, HttpError> {
        let town = format!("{postcode} {city}").trim().to_string();
        let address = [street, town.as_str(), country]
            .iter()
            .filter(|part| !part.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        Ok(self.lookup(&address)?.map(|hit| (hit.lat, hit.lon)))
    }

    fn reverse(&self, lat: f64, lon: f64) -> Result<Option<Address>, HttpError> {
        let params = [
            ("latlng", format!("{lat},{lon}")),
            ("result_type", "street_address|premise".to_string()),
        ];
        let results = self.request_results(&params)?;
        let Some(first) = results.first() else {
            return Ok(None);
        };
        let components = Self::address_components_by_type(first);
        let street = [components.get("route"), components.get("street_number")]
            .into_iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        if street.is_empty() {
            return Ok(None);
        }
        let city = components
            .get("locality")
            .or_else(|| components.get("postal_town"))
            .cloned()
            .unwrap_or_default();
        let postcode = components.get("postal_code").cloned().unwrap_or_default();
        Ok(Some((street, postcode, city)))
    }
}

/// The geocoder selected with `--geocoder`.
pub fn make_geocoder(
    kind: &str,
    country_code: &str,
    http: Arc<Http>,
) -> Result<Box<dyn Geocoder>, String> {
    match kind {
        "google" => Ok(Box::new(Google::new(http, country_code)?)),
        "nominatim" => Ok(Box::new(Nominatim::new(http, country_code))),
        other => Err(format!("unknown geocoder '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::testing::serve;

    #[test]
    fn nominatim_search_results_become_hits() {
        let server = serve(vec![(
            200,
            r#"[{"lat":"47.07","lon":"15.44","display_name":"Hauptplatz, Graz","addresstype":"square"}]"#,
        )]);
        let mut http = Http::new("http://unused".into(), None);
        http.backoff_ms = 1;
        let geocoder = Nominatim::new(Arc::new(http), "at");
        // The test server stands in for Nominatim: fetch the canned answer and parse it.
        let answer = geocoder
            .http
            .get_json(&format!("{}/search?q=x", server.url))
            .unwrap();
        let hit = Nominatim::hit_from_search_results(&answer).unwrap();
        assert_eq!((hit.lat, hit.accuracy.as_str()), (47.07, "square"));
        assert!(server.requests.lock().unwrap()[0].starts_with("GET /search?q=x"));
        assert_eq!(
            Nominatim::hit_from_search_results(&serde_json::json!([])),
            None
        );
    }

    #[test]
    fn google_results_become_hits() {
        let result = serde_json::json!({
            "formatted_address": "Hauptplatz 1, 8010 Graz",
            "partial_match": true,
            "geometry": {"location": {"lat": 47.07, "lng": 15.44}, "location_type": "ROOFTOP"}
        });
        let hit = Google::hit_from_result(&result).unwrap();
        assert_eq!((hit.lat, hit.lon), (47.07, 15.44));
        assert_eq!(
            (hit.accuracy.as_str(), hit.partial_match.as_str()),
            ("ROOFTOP", "yes")
        );
        assert_eq!(Google::hit_from_result(&serde_json::json!({})), None);
    }
}
