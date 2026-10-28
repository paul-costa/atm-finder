//! `atm-finder`: ATM locations in Austria from OpenStreetMap, as CSV, GeoJSON or KML.
//!
//! This file is the table of contents of the tool. It defines the command line (clap builds the
//! `--help` texts from the doc comments below) and hands each command to its module:
//!
//! * `export`     -> [`export_cmd`]   places to files
//! * `places`     -> [`export_cmd`]   look up boundary names
//! * `geocode`    -> [`geocode_cmd`]  coordinates for an address list
//! * `batch`      -> [`batch_cmd`]    one file set per place (ready-made files in data/)
//! * `data-index` -> [`batch_cmd`]    the ready-made files for the website
//!
//! The engine itself (boundaries, Overpass queries, merging, export formats) lives in the
//! `atm-finder-core` crate and is shared with the website.

mod addresses;
mod batch_cmd;
mod env;
mod export_cmd;
mod geocode_cmd;
mod geocoders;
mod http;
mod log;
mod sources;

use std::path::PathBuf;
use std::sync::Arc;

use atm_finder_core::boundary::{level_for, LOOKUP_LEVELS};
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::export_cmd::{
    collect, find_candidates, parse_sources, write_outputs, CliError, ExportOptions,
};
use crate::geocoders::make_geocoder;
use crate::http::Http;
use crate::log::{log, set_quiet};

// ------------------------------------------------------------------ command line

#[derive(Parser)]
#[command(
    name = "atm-finder",
    version,
    about = "Export ATM locations in Austria from OpenStreetMap (and optional other sources) as CSV, GeoJSON or KML."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Options every network command accepts.
#[derive(Args, Clone)]
struct Common {
    /// Overpass API endpoint (or ATM_FINDER_OVERPASS_URL)
    #[arg(long, value_name = "URL", default_value_t = http::default_overpass_url())]
    overpass_url: String,
    /// E-mail sent along with Nominatim requests, recommended for many requests (or ATM_FINDER_CONTACT)
    #[arg(long, value_name = "EMAIL")]
    contact: Option<String>,
    /// No progress output on stderr
    #[arg(short, long)]
    quiet: bool,
}

/// The administrative levels a place can be looked up at (OSM `admin_level` 4, 6, 8, 9).
#[derive(Clone, Copy, ValueEnum)]
enum Level {
    State,
    District,
    Municipality,
    Borough,
}

impl Level {
    /// The level's name as used by the core crate (`state`, `district`, ...).
    fn name(self) -> &'static str {
        match self {
            Level::State => "state",
            Level::District => "district",
            Level::Municipality => "municipality",
            Level::Borough => "borough",
        }
    }

    /// The OSM `admin_level` of this level.
    fn admin_level(self) -> u8 {
        level_for(self.name()).expect("every Level has an admin level")
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Csv,
    Geojson,
    Kml,
}

impl Format {
    fn name(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Geojson => "geojson",
            Format::Kml => "kml",
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum GeocoderKind {
    Nominatim,
    Google,
}

impl GeocoderKind {
    fn name(self) -> &'static str {
        match self {
            GeocoderKind::Nominatim => "nominatim",
            GeocoderKind::Google => "google",
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Export the ATMs inside one or more administrative boundaries
    Export(ExportArgs),
    /// List the boundaries whose name contains a text
    Places(PlacesArgs),
    /// Add coordinates to an address list (CSV)
    Geocode(GeocodeArgs),
    /// Export one file set (CSV, GeoJSON, KML) per place into a directory
    Batch(BatchArgs),
    /// Copy the ready-made exports into the website and write index.json
    DataIndex(DataIndexArgs),
}

#[derive(Args)]
struct ExportArgs {
    /// Place name (e.g. Graz, Leoben, Graz-Umgebung, Steiermark), 'Austria', or an OSM relation id such as r1234
    #[arg(required = true, value_name = "PLACE")]
    places: Vec<String>,
    /// Restrict the lookup to one admin level
    #[arg(long, value_enum)]
    level: Option<Level>,
    /// Comma-separated sources: osm, bankaustria, firecrawl
    #[arg(long, value_name = "LIST", default_value = "osm")]
    sources: String,
    /// Output file; the format follows the extension (.csv, .geojson, .kml); repeatable; '-' or none = CSV to stdout
    #[arg(short, long, value_name = "FILE")]
    output: Vec<String>,
    /// Force the output format for every -o
    #[arg(long, value_enum)]
    format: Option<Format>,
    /// Also write the resolved boundary polygon(s) as GeoJSON
    #[arg(long, value_name = "FILE")]
    boundary_out: Option<String>,
    /// Complete missing addresses: nearest OSM house number (<= 50 m), else reverse geocoding
    #[arg(long)]
    fill_addresses: bool,
    /// Geocoder for --fill-addresses and firecrawl addresses
    #[arg(long, value_enum, default_value_t = GeocoderKind::Nominatim)]
    geocoder: GeocoderKind,
    /// ISO country code passed to the geocoder
    #[arg(long, default_value = "at")]
    country_code: String,
    /// Metres within which records of the same operator count as one ATM
    #[arg(long, value_name = "M", default_value_t = atm_finder_core::MERGE_RADIUS_M)]
    merge_radius: f64,
    /// Keep raw records from all sources
    #[arg(long)]
    no_merge: bool,
    /// Search query for the firecrawl source (repeatable)
    #[arg(long, value_name = "TEXT")]
    firecrawl_query: Vec<String>,
    /// Page the firecrawl source should read (repeatable)
    #[arg(long, value_name = "URL")]
    firecrawl_url: Vec<String>,
    /// Results per firecrawl search
    #[arg(long, default_value_t = 10)]
    firecrawl_limit: usize,
    #[command(flatten)]
    common: Common,
}

#[derive(Args)]
struct PlacesArgs {
    /// Text the boundary name must contain
    name: String,
    /// Only boundaries at this admin level
    #[arg(long, value_enum)]
    level: Option<Level>,
    #[command(flatten)]
    common: Common,
}

#[derive(Args)]
struct GeocodeArgs {
    /// CSV file with addresses
    input: String,
    /// Output CSV ('-' = stdout)
    #[arg(short, long, default_value = "-")]
    output: String,
    /// nominatim (free, 1 request/s) or google (GOOGLE_MAPS_API_KEY required)
    #[arg(long, value_enum, default_value_t = GeocoderKind::Nominatim)]
    geocoder: GeocoderKind,
    /// Address column(s) to use, repeatable; joined with ', '
    #[arg(long, value_name = "COLUMN")]
    address_column: Vec<String>,
    /// Restrict matches to this country (empty = worldwide)
    #[arg(long, default_value = "at")]
    country_code: String,
    /// Parallel requests for Google
    #[arg(long, default_value_t = 8)]
    workers: usize,
    /// Also re-geocode rows that already have coordinates
    #[arg(long)]
    force: bool,
    /// Input encoding (e.g. cp1252 for old Excel exports)
    #[arg(long, default_value = "utf-8")]
    encoding: String,
    #[command(flatten)]
    common: Common,
}

#[derive(Args)]
struct BatchArgs {
    /// Comma-separated places (Name or Name|level); default: the places file
    #[arg(long)]
    places: Option<String>,
    /// File with one place per line (Name or Name|level, # comments)
    #[arg(long, default_value = "data/places.txt")]
    places_file: PathBuf,
    /// Directory for the files
    #[arg(long, default_value = "data")]
    out_dir: PathBuf,
    /// Admin level applied to every place without its own
    #[arg(long, value_enum)]
    level: Option<Level>,
    /// Do not complete missing addresses
    #[arg(long)]
    no_fill_addresses: bool,
    /// Append a Markdown summary to this file (e.g. $GITHUB_STEP_SUMMARY)
    #[arg(long)]
    summary: Option<PathBuf>,
    #[command(flatten)]
    common: Common,
}

#[derive(Args)]
struct DataIndexArgs {
    /// Directory with the exports (data/)
    src: PathBuf,
    /// Destination inside the website (web/dist/data)
    dst: PathBuf,
}

// ------------------------------------------------------------------ commands

/// The shared HTTP client, configured from the common options.
fn http_client(common: &Common) -> Arc<Http> {
    set_quiet(common.quiet);
    Arc::new(Http::new(
        common.overpass_url.clone(),
        common.contact.clone(),
    ))
}

fn run_export(args: ExportArgs) -> Result<(), CliError> {
    let http = http_client(&args.common);
    let sources = parse_sources(&args.sources)?;
    // Check the output extensions before any network request, so a typo fails fast.
    if args.format.is_none() {
        for path in args.output.iter().filter(|path| *path != "-") {
            export_cmd::infer_format(path)?;
        }
    }
    let options = ExportOptions {
        sources,
        fill_addresses: args.fill_addresses,
        geocoder: args.geocoder.name().to_string(),
        country_code: args.country_code,
        merge_radius: args.merge_radius,
        no_merge: args.no_merge,
        firecrawl_query: args.firecrawl_query,
        firecrawl_url: args.firecrawl_url,
        firecrawl_limit: args.firecrawl_limit,
    };
    let level = args.level.map(Level::admin_level);
    let collected = collect(&http, &args.places, level, &options)?;
    let format = args.format.map(Format::name);
    write_outputs(
        &collected,
        &args.output,
        format,
        args.boundary_out.as_deref(),
    )
}

fn run_places(args: PlacesArgs) -> Result<i32, CliError> {
    let http = http_client(&args.common);
    let levels: Vec<u8> = match args.level {
        Some(level) => vec![level.admin_level()],
        None => LOOKUP_LEVELS.to_vec(),
    };
    let found = find_candidates(&http, &args.name, &levels, false)?;
    if found.is_empty() {
        log(&format!(
            "no administrative boundary whose name contains '{}' found in Austria",
            args.name
        ));
        return Ok(1);
    }
    println!(
        "{:<12}{:<14}{:<30}{:<18}{:<8}url",
        "relation", "level", "name", "state", "GKZ"
    );
    for boundary in found {
        println!(
            "r{:<11}{:<14}{:<30}{:<18}{:<8}{}",
            boundary.relation_id,
            boundary.level_key(),
            boundary.name,
            boundary.state(),
            boundary.gkz(),
            boundary.url()
        );
    }
    Ok(0)
}

fn run_geocode(args: GeocodeArgs) -> Result<(), CliError> {
    let http = http_client(&args.common);
    let geocoder = make_geocoder(args.geocoder.name(), &args.country_code, http)?;
    let options = geocode_cmd::GeocodeOptions {
        input: &args.input,
        output: &args.output,
        address_columns: &args.address_column,
        workers: args.workers,
        force: args.force,
        encoding: &args.encoding,
    };
    geocode_cmd::run(&options, geocoder.as_ref())?;
    Ok(())
}

fn run_batch(args: BatchArgs) -> Result<i32, CliError> {
    let http = http_client(&args.common);
    let options = batch_cmd::BatchOptions {
        places: args.places,
        places_file: args.places_file,
        out_dir: args.out_dir,
        level: args.level.map(|level| level.name().to_string()),
        fill_addresses: !args.no_fill_addresses,
        summary: args.summary,
    };
    batch_cmd::run_batch(&http, &options)
}

fn main() {
    let cli = Cli::parse();
    let result: Result<i32, CliError> = match cli.command {
        Command::Export(args) => run_export(args).map(|()| 0),
        Command::Places(args) => run_places(args),
        Command::Geocode(args) => run_geocode(args).map(|()| 0),
        Command::Batch(args) => run_batch(args),
        Command::DataIndex(args) => batch_cmd::run_data_index(&args.src, &args.dst).map(|()| 0),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            // An empty message means the details were already printed (e.g. candidate lists).
            if !error.message.is_empty() {
                eprintln!("error: {}", error.message);
            }
            std::process::exit(error.code);
        }
    }
}
