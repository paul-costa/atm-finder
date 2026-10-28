//! Two helpers for building the website's ready-made files.
//!
//! `atm-finder batch` exports one file set (CSV, GeoJSON, KML) per place of a list into `data/`
//! (on costaserver through `server/deploy.sh --export`). `atm-finder data-index` copies those files into
//! the website and writes the `index.json` the page reads for its "Ready-made files" section.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use atm_finder_core::boundary::level_for;
use atm_finder_core::places::{read_places, slugify};

use crate::export_cmd::{collect, write_outputs, CliError, ExportOptions};
use crate::http::Http;
use crate::log::log;

// ------------------------------------------------------------------ batch

/// The arguments of `atm-finder batch`.
pub struct BatchOptions {
    /// Comma-separated places; when empty the places file is read.
    pub places: Option<String>,
    /// One place per line, `Name` or `Name|level`, `#` comments.
    pub places_file: PathBuf,
    pub out_dir: PathBuf,
    /// Level for places that do not carry their own (`state`, `district`, ...).
    pub level: Option<String>,
    pub fill_addresses: bool,
    /// A Markdown table of the results is appended to this file (GitHub's step summary).
    pub summary: Option<PathBuf>,
}

/// Export every place and report a results table. Returns the exit code: 1 when any place
/// failed, so the workflow run is marked, while the successful files are still written.
pub fn run_batch(http: &Arc<Http>, opts: &BatchOptions) -> Result<i32, CliError> {
    let places = places_to_export(opts)?;
    std::fs::create_dir_all(&opts.out_dir)
        .map_err(|e| CliError::new(format!("creating {}: {e}", opts.out_dir.display())))?;

    let mut table = vec![
        "| place | level | rows | files |".to_string(),
        "|---|---|---|---|".to_string(),
    ];
    let mut failures = 0;
    for (name, own_level) in places {
        let level = if own_level.is_empty() {
            opts.level.clone().unwrap_or_default()
        } else {
            own_level
        };
        let level_label = if level.is_empty() {
            "auto"
        } else {
            level.as_str()
        };
        let slug = if level.is_empty() {
            slugify(&name)
        } else {
            format!("{}-{level}", slugify(&name))
        };
        log(&format!("== {name} ({level_label}) -> {slug}"));
        match export_one_place(http, &name, &level, &slug, opts) {
            Ok((rows, files)) => {
                table.push(format!(
                    "| {name} | {level_label} | {rows} | {} |",
                    files.join(", ")
                ));
            }
            Err(error) => {
                if !error.message.is_empty() {
                    eprintln!("error: {}", error.message);
                }
                failures += 1;
                table.push(format!("| {name} | {level_label} | failed | |"));
            }
        }
    }

    if let Some(path) = &opts.summary {
        append_summary(path, &table)?;
    }
    println!("{}", table.join("\n"));
    Ok(if failures > 0 { 1 } else { 0 })
}

/// The places from `--places` or from the places file.
fn places_to_export(opts: &BatchOptions) -> Result<Vec<(String, String)>, CliError> {
    let text = match &opts.places {
        Some(list) if !list.trim().is_empty() => list.clone(),
        _ => std::fs::read_to_string(&opts.places_file)
            .map_err(|e| CliError::new(format!("reading {}: {e}", opts.places_file.display())))?,
    };
    let places = read_places(&text);
    if places.is_empty() {
        return Err(CliError::usage("no places given"));
    }
    Ok(places)
}

/// Export one place as `<slug>.csv`, `.geojson` and `.kml` in the output directory. Returns
/// the number of ATMs and the file names.
fn export_one_place(
    http: &Arc<Http>,
    name: &str,
    level: &str,
    slug: &str,
    opts: &BatchOptions,
) -> Result<(usize, Vec<String>), CliError> {
    let admin_level = if level.is_empty() {
        None
    } else {
        let parsed = level_for(level)
            .ok_or_else(|| CliError::usage(format!("unknown level '{level}' for {name}")))?;
        Some(parsed)
    };
    let file_names: Vec<String> = ["csv", "geojson", "kml"]
        .iter()
        .map(|extension| format!("{slug}.{extension}"))
        .collect();
    let paths: Vec<String> = file_names
        .iter()
        .map(|file| opts.out_dir.join(file).display().to_string())
        .collect();
    let export_opts = ExportOptions {
        sources: vec!["osm".to_string()],
        fill_addresses: opts.fill_addresses,
        geocoder: "nominatim".to_string(),
        country_code: "at".to_string(),
        merge_radius: atm_finder_core::MERGE_RADIUS_M,
        no_merge: false,
        firecrawl_query: Vec::new(),
        firecrawl_url: Vec::new(),
        firecrawl_limit: 10,
    };
    let collected = collect(
        http,
        std::slice::from_ref(&name.to_string()),
        admin_level,
        &export_opts,
    )?;
    write_outputs(&collected, &paths, None, None)?;
    Ok((collected.atms.len(), file_names))
}

fn append_summary(path: &Path, table: &[String]) -> Result<(), CliError> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| CliError::new(format!("writing {}: {e}", path.display())))?;
    writeln!(file, "## ATM export\n\n{}", table.join("\n"))
        .map_err(|e| CliError::new(e.to_string()))
}

// ------------------------------------------------------------------ data index

/// The calendar date (`YYYY-MM-DD`, UTC) of a Unix timestamp, without a date library.
/// Algorithm "days to civil" by Howard Hinnant.
pub fn iso_date(unix_seconds: i64) -> String {
    let days_since_epoch = unix_seconds.div_euclid(86_400);
    let z = days_since_epoch + 719_468; // shift the epoch to 0000-03-01
    let era = z.div_euclid(146_097); // 400-year cycles
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153; // 0 = March
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };
    format!("{year:04}-{month:02}-{day:02}")
}

/// Copy the `.csv`/`.geojson`/`.kml` files of `src` into `dst` and write `dst/index.json`
/// with one entry per data set: slug, place name, level, row count, files, date.
pub fn run_data_index(src: &Path, dst: &Path) -> Result<(), CliError> {
    std::fs::create_dir_all(dst)
        .map_err(|e| CliError::new(format!("creating {}: {e}", dst.display())))?;
    let mut entries = Vec::new();
    for csv_path in csv_files(src)? {
        let slug = csv_path.file_stem().unwrap().to_string_lossy().to_string();
        let (rows, place) = rows_and_place(&csv_path)?;
        let files = copy_data_set(src, dst, &slug)?;
        entries.push(serde_json::json!({
            "slug": slug,
            "name": place.unwrap_or_else(|| slug.clone()),
            "level": level_from_slug(&slug),
            "rows": rows,
            "files": files,
            "updated": modified_date(&csv_path),
        }));
    }
    let json = serde_json::to_string_pretty(&entries).unwrap_or_default();
    std::fs::write(dst.join("index.json"), json + "\n")
        .map_err(|e| CliError::new(e.to_string()))?;
    log(&format!(
        "{} data sets -> {}",
        entries.len(),
        dst.join("index.json").display()
    ));
    Ok(())
}

/// The `.csv` files of a directory, sorted by name.
fn csv_files(dir: &Path) -> Result<Vec<PathBuf>, CliError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| CliError::new(format!("reading {}: {e}", dir.display())))?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("csv"))
        .collect();
    files.sort();
    Ok(files)
}

/// Row count and the `place` of the first row of an exported CSV.
fn rows_and_place(csv_path: &Path) -> Result<(usize, Option<String>), CliError> {
    let text = std::fs::read_to_string(csv_path).map_err(|e| CliError::new(e.to_string()))?;
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(text.as_bytes());
    let place_column = reader
        .headers()
        .ok()
        .and_then(|header| header.iter().position(|column| column == "place"));
    let mut rows = 0;
    let mut place = None;
    for record in reader.records().flatten() {
        if rows == 0 {
            place = place_column
                .and_then(|i| record.get(i))
                .filter(|p| !p.is_empty())
                .map(String::from);
        }
        rows += 1;
    }
    Ok((rows, place))
}

/// The level encoded in a slug such as `graz-umgebung-district`; empty for plain names.
fn level_from_slug(slug: &str) -> String {
    ["state", "district", "municipality", "borough"]
        .iter()
        .find(|level| slug.ends_with(&format!("-{level}")))
        .map(|level| level.to_string())
        .unwrap_or_default()
}

/// Copy the files of one data set that exist; returns `{"csv": "<slug>.csv", ...}`.
fn copy_data_set(
    src: &Path,
    dst: &Path,
    slug: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, CliError> {
    let mut files = serde_json::Map::new();
    for extension in ["csv", "geojson", "kml"] {
        let file_name = format!("{slug}.{extension}");
        let source = src.join(&file_name);
        if !source.exists() {
            continue;
        }
        std::fs::copy(&source, dst.join(&file_name))
            .map_err(|e| CliError::new(format!("copying {}: {e}", source.display())))?;
        files.insert(extension.to_string(), serde_json::Value::String(file_name));
    }
    Ok(files)
}

/// The modification date of a file as `YYYY-MM-DD`, empty when unknown.
fn modified_date(path: &Path) -> String {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since_epoch| iso_date(since_epoch.as_secs() as i64))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(iso_date(0), "1970-01-01");
        assert_eq!(iso_date(1_790_000_000), "2026-09-21");
        assert_eq!(iso_date(951_782_400), "2000-02-29");
    }

    #[test]
    fn slugs_carry_the_level() {
        assert_eq!(level_from_slug("graz"), "");
        assert_eq!(level_from_slug("graz-umgebung-district"), "district");
        assert_eq!(level_from_slug("salzburg-state"), "state");
    }

    #[test]
    fn data_index_from_csv_files() {
        let dir = std::env::temp_dir().join(format!("atm-finder-index-{}", std::process::id()));
        let (src, dst) = (dir.join("src"), dir.join("dst"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("graz.csv"),
            "id,place\nosm:node/1,Graz\nosm:node/2,Graz\n",
        )
        .unwrap();
        std::fs::write(src.join("graz.geojson"), "{}").unwrap();
        std::fs::write(
            src.join("gu-district.csv"),
            "id,place\nosm:node/3,Bezirk Graz-Umgebung\n",
        )
        .unwrap();
        run_data_index(&src, &dst).unwrap();
        let index: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dst.join("index.json")).unwrap())
                .unwrap();
        assert_eq!(index[0]["slug"], "graz");
        assert_eq!(index[0]["rows"], 2);
        assert_eq!(index[0]["files"]["geojson"], "graz.geojson");
        assert_eq!(index[1]["name"], "Bezirk Graz-Umgebung");
        assert_eq!(index[1]["level"], "district");
        assert!(dst.join("graz.csv").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
