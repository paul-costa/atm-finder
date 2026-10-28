//! `atm-finder export`: from place names to files.
//!
//! The steps, in order: resolve every place to a boundary (an OSM relation with its polygon),
//! fetch the raw records of each requested source, merge records that describe the same ATM,
//! optionally complete missing addresses, fill in the city where it is obvious, sort, and write
//! the files. `batch` reuses the same steps for one place at a time.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use atm_finder_core::boundary::{self, Boundary, AUSTRIA_RELATION, LOOKUP_LEVELS};
use atm_finder_core::export;
use atm_finder_core::model::{merge, Atm};
use atm_finder_core::sources::SOURCES;

use crate::addresses::fill_addresses;
use crate::geocoders::{make_geocoder, Geocoder};
use crate::http::{Http, HttpError};
use crate::log::log;
use crate::sources::{fetch_bankaustria, fetch_firecrawl, fetch_osm, FirecrawlOptions};

/// Exit code for a failed run (network, nothing found, ...).
pub const EXIT_ERROR: i32 = 1;
/// Exit code for wrong usage (unknown option values, an ambiguous place).
pub const EXIT_USAGE: i32 = 2;

/// Names that mean the whole country.
const COUNTRY_ALIASES: [&str; 6] = [
    "austria",
    "österreich",
    "oesterreich",
    "osterreich",
    "at",
    "aut",
];
/// How many similar names to list when a place is not found.
const MAX_SUGGESTIONS: usize = 15;

/// An error together with the exit code the process should end with.
///
/// An empty message means the details were already printed (e.g. the list of candidates for an
/// ambiguous place) and `main` should only exit.
#[derive(Debug)]
pub struct CliError {
    pub message: String,
    pub code: i32,
}

impl CliError {
    pub fn new(message: impl Into<String>) -> Self {
        CliError {
            message: message.into(),
            code: EXIT_ERROR,
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        CliError {
            message: message.into(),
            code: EXIT_USAGE,
        }
    }
}

impl From<HttpError> for CliError {
    fn from(e: HttpError) -> Self {
        CliError::new(e.message)
    }
}

impl From<String> for CliError {
    fn from(message: String) -> Self {
        CliError::new(message)
    }
}

// ------------------------------------------------------------------ resolving places

/// The relation id in `r1234`, `rel1234`, `relation/1234` or plain `1234`; `None` for names.
pub fn parse_relation_spec(spec: &str) -> Option<u64> {
    let lower = spec.trim().to_lowercase();
    let digits = ["relation/", "relation", "rel", "r"]
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix))
        .unwrap_or(&lower);
    let all_digits = !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit());
    if all_digits {
        digits.parse().ok()
    } else {
        None
    }
}

/// Boundaries in Austria whose name equals `name` (`exact`) or contains it, at the given
/// admin levels, sorted by level and name. Without geometry.
pub fn find_candidates(
    http: &Http,
    name: &str,
    levels: &[u8],
    exact: bool,
) -> Result<Vec<Boundary>, HttpError> {
    let answer = http.overpass(&boundary::candidates_query(name, levels, exact))?;
    if let Some(remark) = answer.get("remark").and_then(|r| r.as_str()) {
        log(&format!("  Overpass remark: {remark}"));
    }
    let candidates = boundary::parse_candidates(&answer);
    if http.debug {
        for candidate in &candidates {
            log(&format!("  candidate: {}", candidate.describe()));
        }
    }
    Ok(candidates)
}

/// Fetch the relation's member ways and attach the assembled polygon. Name, level and tags are
/// filled in from the relation when the boundary was given as a bare id.
pub fn load_geometry(http: &Http, boundary: &mut Boundary) -> Result<(), CliError> {
    let answer = http.overpass(&boundary::geometry_query(boundary.relation_id))?;
    let (tags, geometry) = boundary::parse_geometry(&answer, boundary.relation_id)?;
    if boundary.tags.is_empty() {
        boundary.tags = tags;
    }
    if boundary.admin_level == 0 {
        boundary.admin_level = boundary
            .tags
            .get("admin_level")
            .and_then(|level| level.parse().ok())
            .unwrap_or(0);
    }
    if boundary.name.is_empty() {
        boundary.name = boundary
            .tags
            .get("name")
            .cloned()
            .unwrap_or_else(|| format!("relation {}", boundary.relation_id));
    }
    boundary.geometry = Some(geometry);
    Ok(())
}

/// Turn what the user typed into a boundary with geometry.
///
/// Accepted are a relation id (`r34719`), a country alias (`Austria`) and a name. A name is
/// matched exactly first; when nothing matches, names that differ only by a prefix such as
/// "Bezirk " or by "St." versus "Sankt" are accepted (districts are tagged "Bezirk X" in OSM).
/// Several boundaries at the preferred level are reported as ambiguous with exit code 2.
pub fn resolve(http: &Http, spec: &str, level: Option<u8>) -> Result<Boundary, CliError> {
    let spec = spec.trim();
    if let Some(relation_id) = parse_relation_spec(spec) {
        return boundary_from_relation(http, relation_id, "", 0);
    }
    if COUNTRY_ALIASES.contains(&spec.to_lowercase().as_str()) {
        return boundary_from_relation(http, AUSTRIA_RELATION, "Austria", 2);
    }
    let levels: Vec<u8> = match level {
        Some(level) => vec![level],
        None => LOOKUP_LEVELS.to_vec(),
    };
    let mut matches = find_candidates(http, spec, &levels, true)?;
    if matches.is_empty() {
        matches = lenient_matches(http, spec, &levels, level)?;
    }
    match boundary::pick(&matches) {
        Ok(mut chosen) => {
            load_geometry(http, &mut chosen)?;
            Ok(chosen)
        }
        Err(same_level) => {
            eprintln!(
                "'{spec}' matches several boundaries; pick one with --level or by relation id:"
            );
            print_candidates(&same_level);
            Err(CliError::usage(String::new()))
        }
    }
}

/// A boundary for a known relation id, with its geometry loaded.
fn boundary_from_relation(
    http: &Http,
    relation_id: u64,
    name: &str,
    admin_level: u8,
) -> Result<Boundary, CliError> {
    let mut boundary = Boundary {
        relation_id,
        name: name.to_string(),
        admin_level,
        ..Default::default()
    };
    load_geometry(http, &mut boundary)?;
    Ok(boundary)
}

/// Second try for a name without an exact match: boundaries whose normalised name equals the
/// normalised input ("Graz-Umgebung" finds "Bezirk Graz-Umgebung"). When there is none either,
/// the error lists the names that contain the input so the user can pick one.
fn lenient_matches(
    http: &Http,
    spec: &str,
    levels: &[u8],
    level: Option<u8>,
) -> Result<Vec<Boundary>, CliError> {
    let similar = find_candidates(http, spec, levels, false)?;
    let wanted = boundary::normalize_name(spec);
    let matches: Vec<Boundary> = similar
        .iter()
        .filter(|candidate| boundary::normalize_name(&candidate.name) == wanted)
        .cloned()
        .collect();
    if matches.is_empty() {
        return Err(not_found(spec, level, &similar));
    }
    if matches.len() == 1 {
        log(&format!(
            "  no exact match for '{spec}', using '{}'",
            matches[0].name
        ));
    } else {
        log(&format!(
            "  no exact match for '{spec}', {} similar names",
            matches.len()
        ));
    }
    Ok(matches)
}

/// The error for a name that matches nothing, with suggestions when there are similar names.
fn not_found(spec: &str, level: Option<u8>, similar: &[Boundary]) -> CliError {
    let level_hint = level
        .map(|l| format!(" at level '{}'", boundary::level_key(l)))
        .unwrap_or_default();
    let mut message =
        format!("no administrative boundary named '{spec}'{level_hint} found in Austria");
    if similar.is_empty() {
        message.push_str(" (try `atm-finder places NAME` or a relation id)");
    } else {
        message.push_str("; names containing it:");
        for candidate in similar.iter().take(MAX_SUGGESTIONS) {
            message.push_str(&format!("\n  {}", candidate.describe()));
        }
    }
    CliError::new(message)
}

/// Print candidates as a table on stderr (for ambiguous names).
fn print_candidates(candidates: &[Boundary]) {
    for candidate in candidates {
        eprintln!(
            "  r{:<10} {:<13} {:<30} {:<18} {}",
            candidate.relation_id,
            candidate.level_key(),
            candidate.name,
            candidate.state(),
            candidate.url()
        );
    }
}

// ------------------------------------------------------------------ collecting records

/// Everything `export` needs to know besides the places.
pub struct ExportOptions {
    /// Source names, validated by [`parse_sources`].
    pub sources: Vec<String>,
    pub fill_addresses: bool,
    /// `nominatim` or `google`; only built when a source or `fill_addresses` needs it.
    pub geocoder: String,
    pub country_code: String,
    pub merge_radius: f64,
    pub no_merge: bool,
    pub firecrawl_query: Vec<String>,
    pub firecrawl_url: Vec<String>,
    pub firecrawl_limit: usize,
}

/// The outcome of [`collect`]: ready-to-write ATMs and the boundaries they belong to.
pub struct Collected {
    pub atms: Vec<Atm>,
    pub boundaries: Vec<Boundary>,
    /// Kept for the cost summary (Google requests).
    pub geocoder: Option<Box<dyn Geocoder>>,
}

/// Split and validate the `--sources` list.
pub fn parse_sources(list: &str) -> Result<Vec<String>, CliError> {
    let sources: Vec<String> = list
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let unknown: Vec<&str> = sources
        .iter()
        .map(String::as_str)
        .filter(|s| !SOURCES.contains(s))
        .collect();
    if !unknown.is_empty() {
        return Err(CliError::usage(format!(
            "unknown source(s): {} (choose from {})",
            unknown.join(", "),
            SOURCES.join(", ")
        )));
    }
    Ok(sources)
}

/// Resolve every place, gather and merge its records, then complete addresses and sort.
pub fn collect(
    http: &Arc<Http>,
    specs: &[String],
    level: Option<u8>,
    opts: &ExportOptions,
) -> Result<Collected, CliError> {
    // The geocoder is only built when needed, so `export Graz` never asks for a Google key.
    let needs_geocoder = opts.fill_addresses || opts.sources.iter().any(|s| s == "firecrawl");
    let geocoder: Option<Box<dyn Geocoder>> = if needs_geocoder {
        Some(make_geocoder(
            &opts.geocoder,
            &opts.country_code,
            http.clone(),
        )?)
    } else {
        None
    };

    let mut boundaries = Vec::new();
    for spec in specs {
        log(&format!("Resolving '{spec}' ..."));
        let boundary = resolve(http, spec, level)?;
        log(&format!("  {}", boundary.describe()));
        boundaries.push(boundary);
    }

    let mut atms: Vec<Atm> = Vec::new();
    for boundary in &boundaries {
        let records = fetch_records(http, boundary, opts, geocoder.as_deref());
        if records.is_empty() {
            log(&format!("  no ATMs found in {}", boundary.name));
            continue;
        }
        let raw_count = records.len();
        let merged = if opts.no_merge {
            records
        } else {
            merge(records, opts.merge_radius)
        };
        log(&format!(
            "  {} ATMs in {} ({raw_count} raw records)",
            merged.len(),
            boundary.name
        ));
        atms.extend(merged);
    }
    if atms.is_empty() {
        return Err(CliError::new("no results"));
    }

    if opts.fill_addresses {
        let geocoder = geocoder
            .as_deref()
            .expect("geocoder is built when fill_addresses is set");
        let counts = fill_addresses(http, geocoder, &mut atms);
        log(&format!(
            "  addresses: {} from nearby house numbers, {} reverse geocoded, {} unresolved",
            counts.nearby, counts.reverse, counts.unresolved
        ));
    }
    apply_default_city(&mut atms, &boundaries);
    export::sort_rows(&mut atms);
    Ok(Collected {
        atms,
        boundaries,
        geocoder,
    })
}

/// The raw records of every requested source for one boundary. A failing source is reported
/// and skipped, so one broken bank website does not spoil the OSM export.
fn fetch_records(
    http: &Http,
    boundary: &Boundary,
    opts: &ExportOptions,
    geocoder: Option<&dyn Geocoder>,
) -> Vec<Atm> {
    let mut records = Vec::new();
    for source in &opts.sources {
        let started = Instant::now();
        log(&format!("Source {source} for {} ...", boundary.name));
        let fetched: Result<Vec<Atm>, String> = match source.as_str() {
            "osm" => fetch_osm(http, boundary).map_err(|e| e.message),
            "bankaustria" => fetch_bankaustria(http, boundary).map_err(|e| e.message),
            _ => match geocoder {
                Some(geocoder) => {
                    let firecrawl_opts = FirecrawlOptions {
                        queries: &opts.firecrawl_query,
                        urls: &opts.firecrawl_url,
                        limit: opts.firecrawl_limit,
                        country: "Austria",
                    };
                    fetch_firecrawl(http, boundary, geocoder, &firecrawl_opts)
                }
                None => Err("firecrawl needs a geocoder".to_string()),
            },
        };
        match fetched {
            Ok(found) => {
                log(&format!(
                    "  {} records in {:.1}s",
                    found.len(),
                    started.elapsed().as_secs_f64()
                ));
                records.extend(found);
            }
            Err(e) => log(&format!("  ERROR in source {source}: {e}")),
        }
    }
    records
}

/// Use the boundary's name as city where a record has none, but only for municipalities.
/// For a district or a state the boundary name would be wrong as a city, and a statutory city
/// such as Graz is recognised by its five-digit municipality code.
fn apply_default_city(atms: &mut [Atm], boundaries: &[Boundary]) {
    for atm in atms.iter_mut() {
        if !atm.city.is_empty() {
            continue;
        }
        let own_boundary = boundaries.iter().find(|b| b.name == atm.place);
        if let Some(boundary) = own_boundary {
            if boundary.is_municipality() {
                atm.city = boundary.name.clone();
            }
        }
    }
}

// ------------------------------------------------------------------ writing files

/// The output format implied by a file extension (`.csv`, `.geojson`/`.json`, `.kml`).
pub fn infer_format(path: &str) -> Result<&'static str, CliError> {
    let extension = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match extension.as_str() {
        "csv" => Ok("csv"),
        "geojson" | "json" => Ok("geojson"),
        "kml" => Ok("kml"),
        _ => Err(CliError::usage(format!(
            "cannot infer the format of '{path}'; use --format csv|geojson|kml"
        ))),
    }
}

/// Write text to a file, or to stdout for `-`.
fn write_text(path: &str, text: &str) -> Result<(), CliError> {
    if path == "-" {
        use std::io::Write;
        std::io::stdout()
            .write_all(text.as_bytes())
            .map_err(|e| CliError::new(format!("writing stdout: {e}")))
    } else {
        std::fs::write(path, text).map_err(|e| CliError::new(format!("writing {path}: {e}")))
    }
}

/// Render the ATMs in one format. The boundary polygon is embedded in GeoJSON when exactly one
/// place was exported.
fn render(collected: &Collected, format: &str) -> String {
    match format {
        "csv" => export::to_csv(&collected.atms),
        "geojson" => {
            let single_boundary = match collected.boundaries.as_slice() {
                [only] => Some(only),
                _ => None,
            };
            export::to_geojson(&collected.atms, single_boundary)
        }
        _ => {
            let names: Vec<&str> = collected
                .boundaries
                .iter()
                .map(|b| b.name.as_str())
                .collect();
            export::to_kml(&collected.atms, &format!("ATMs {}", names.join(", ")))
        }
    }
}

/// Write the ATMs to every output path (the format follows the extension unless `format`
/// forces one; no path means CSV on stdout), plus the boundaries as GeoJSON when asked for.
pub fn write_outputs(
    collected: &Collected,
    outputs: &[String],
    format: Option<&str>,
    boundary_out: Option<&str>,
) -> Result<(), CliError> {
    let stdout_only = vec!["-".to_string()];
    let outputs = if outputs.is_empty() {
        &stdout_only
    } else {
        outputs
    };
    for path in outputs {
        let format = match format {
            Some(forced) => forced,
            None if path == "-" => "csv",
            None => infer_format(path)?,
        };
        write_text(path, &render(collected, format))?;
        log(&format!(
            "Wrote {} ATMs as {format} -> {path}",
            collected.atms.len()
        ));
    }
    if let Some(path) = boundary_out {
        let features: Vec<serde_json::Value> = collected
            .boundaries
            .iter()
            .map(|b| b.to_feature())
            .collect();
        let collection = serde_json::json!({"type": "FeatureCollection", "features": features});
        write_text(
            path,
            &serde_json::to_string(&collection).unwrap_or_default(),
        )?;
        log(&format!(
            "Wrote {} boundary polygon(s) -> {path}",
            collected.boundaries.len()
        ));
    }
    if let Some(geocoder) = &collected.geocoder {
        if geocoder.name() == "google" {
            log(&format!(
                "{} Google requests (10,000/month free, then USD 5 per 1,000)",
                geocoder.requests()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relation_specs() {
        assert_eq!(parse_relation_spec("r34719"), Some(34719));
        assert_eq!(parse_relation_spec("relation/16239"), Some(16239));
        assert_eq!(parse_relation_spec("16239"), Some(16239));
        assert_eq!(parse_relation_spec("Graz"), None);
        assert_eq!(parse_relation_spec("r"), None);
    }

    #[test]
    fn sources_and_formats() {
        assert_eq!(
            parse_sources("osm, bankaustria").unwrap(),
            vec!["osm", "bankaustria"]
        );
        assert_eq!(parse_sources("osm,web").unwrap_err().code, EXIT_USAGE);
        assert_eq!(infer_format("x/graz.GeoJSON").unwrap(), "geojson");
        assert_eq!(infer_format("a.kml").unwrap(), "kml");
        assert!(infer_format("a.txt").is_err());
    }

    #[test]
    fn not_found_lists_similar_names() {
        let similar = vec![Boundary {
            relation_id: 1,
            name: "Sankt Veit an der Glan".into(),
            admin_level: 8,
            ..Default::default()
        }];
        let error = not_found("Veit", Some(8), &similar);
        assert_eq!(error.code, EXIT_ERROR);
        assert!(error.message.contains("at level 'municipality'"));
        assert!(error
            .message
            .contains("Sankt Veit an der Glan (municipality, relation 1)"));
        assert!(not_found("x", None, &[])
            .message
            .contains("atm-finder places"));
    }

    #[test]
    fn default_city_only_for_municipalities() {
        let mut graz = Boundary {
            relation_id: 1,
            name: "Graz".into(),
            admin_level: 6,
            ..Default::default()
        };
        graz.tags.insert("ref:at:gkz".into(), "60101".into());
        let district = Boundary {
            relation_id: 2,
            name: "Bezirk Graz-Umgebung".into(),
            admin_level: 6,
            ..Default::default()
        };
        let mut atms = vec![
            Atm {
                place: "Graz".into(),
                ..Default::default()
            },
            Atm {
                place: "Bezirk Graz-Umgebung".into(),
                ..Default::default()
            },
            Atm {
                place: "Graz".into(),
                city: "Andritz".into(),
                ..Default::default()
            },
        ];
        apply_default_city(&mut atms, &[graz, district]);
        let cities: Vec<&str> = atms.iter().map(|a| a.city.as_str()).collect();
        assert_eq!(cities, vec!["Graz", "", "Andritz"]);
    }
}
