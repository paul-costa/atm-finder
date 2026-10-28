//! `atm-finder geocode`: add coordinates to any CSV of addresses.
//!
//! The command reads the file, detects the delimiter and the address columns, looks every
//! distinct address up once (in parallel for Google, one per second for Nominatim), and writes
//! the same table back with `lat`, `lon` and five `geo_*` columns describing the result.
//! Rows that already have coordinates are left alone unless `--force` is given, so a list can be
//! completed in several runs without spending requests twice.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::export_cmd::CliError;
use crate::geocoders::{Geocoder, Hit};
use crate::http::HttpError;
use crate::log::log;

/// The answer per distinct address: found, not found, or the error that stopped the lookup.
type LookupResults = HashMap<String, Result<Option<Hit>, HttpError>>;

// Column names (lower-cased) that are recognised automatically, English and German.
const SINGLE: [&str; 5] = [
    "address",
    "adresse",
    "anschrift",
    "full_address",
    "vollständige adresse",
];
const STREET: [&str; 6] = ["street", "straße", "strasse", "str", "str.", "addr:street"];
const HOUSENUMBER: [&str; 7] = [
    "housenumber",
    "house_number",
    "hausnummer",
    "hnr",
    "nr",
    "nr.",
    "addr:housenumber",
];
const POSTCODE: [&str; 6] = [
    "postcode",
    "plz",
    "zip",
    "postleitzahl",
    "postal_code",
    "addr:postcode",
];
const CITY: [&str; 6] = ["city", "ort", "stadt", "gemeinde", "town", "addr:city"];
const LAT: [&str; 4] = ["lat", "latitude", "breite", "breitengrad"];
const LON: [&str; 6] = ["lon", "lng", "longitude", "länge", "laenge", "längengrad"];

/// Columns added to the output: what was found, how precise, partial match, status, geocoder.
pub const GEO_COLUMNS: [&str; 5] = [
    "geo_address",
    "geo_accuracy",
    "geo_partial_match",
    "geo_status",
    "geo_source",
];

/// Progress is logged every this many addresses.
const PROGRESS_EVERY: usize = 50;

/// The index of the first header that matches one of `candidates` (case-insensitive).
pub fn find_column(header: &[String], candidates: &[&str]) -> Option<usize> {
    candidates.iter().find_map(|candidate| {
        header
            .iter()
            .position(|h| h.trim().to_lowercase() == *candidate)
    })
}

/// Which columns make up the address of a row.
pub enum AddressColumns {
    /// One column holds the whole address.
    Single(usize),
    /// Street, house number, postcode and city in separate columns (any may be missing).
    Parts {
        street: Option<usize>,
        number: Option<usize>,
        postcode: Option<usize>,
        city: Option<usize>,
    },
    /// Columns named with `--address-column`, joined with ", ".
    Explicit(Vec<usize>),
}

impl AddressColumns {
    /// Detect the address columns, or use the ones given explicitly. Returns the columns and a
    /// short description of them for the log ("Straße + Hausnummer + PLZ + Ort").
    pub fn detect(
        header: &[String],
        explicit: &[String],
    ) -> Result<(AddressColumns, String), CliError> {
        if !explicit.is_empty() {
            return Self::explicit(header, explicit);
        }
        if let Some(index) = find_column(header, &SINGLE) {
            return Ok((AddressColumns::Single(index), header[index].clone()));
        }
        let street = find_column(header, &STREET);
        let number = find_column(header, &HOUSENUMBER);
        let postcode = find_column(header, &POSTCODE);
        let city = find_column(header, &CITY);
        if street.is_none() && postcode.is_none() && city.is_none() {
            return Err(CliError::usage(format!(
                "no address column recognised. Available: {}. Use --address-column.",
                header.join(", ")
            )));
        }
        let used: Vec<String> = [street, number, postcode, city]
            .iter()
            .flatten()
            .map(|&index| header[index].clone())
            .collect();
        let columns = AddressColumns::Parts {
            street,
            number,
            postcode,
            city,
        };
        Ok((columns, used.join(" + ")))
    }

    fn explicit(header: &[String], names: &[String]) -> Result<(AddressColumns, String), CliError> {
        let mut indices = Vec::new();
        let mut missing = Vec::new();
        for name in names {
            match header.iter().position(|h| h == name) {
                Some(index) => indices.push(index),
                None => missing.push(name.clone()),
            }
        }
        if !missing.is_empty() {
            return Err(CliError::usage(format!(
                "column(s) not found: {}. Available: {}",
                missing.join(", "),
                header.join(", ")
            )));
        }
        Ok((AddressColumns::Explicit(indices), names.join(" + ")))
    }

    /// The address text of one row, e.g. "Hauptplatz 1, 8010 Graz"; empty when the row has none.
    pub fn build(&self, row: &[String]) -> String {
        let cell = |index: Option<usize>| -> &str {
            index
                .and_then(|i| row.get(i))
                .map(|value| value.trim())
                .unwrap_or("")
        };
        match self {
            AddressColumns::Single(index) => cell(Some(*index)).to_string(),
            AddressColumns::Explicit(indices) => {
                let parts: Vec<&str> = indices
                    .iter()
                    .map(|&i| cell(Some(i)))
                    .filter(|part| !part.is_empty())
                    .collect();
                parts.join(", ")
            }
            AddressColumns::Parts {
                street,
                number,
                postcode,
                city,
            } => {
                let street_line = join_non_empty(&[cell(*street), cell(*number)], " ");
                let town_line = join_non_empty(&[cell(*postcode), cell(*city)], " ");
                join_non_empty(&[&street_line, &town_line], ", ")
            }
        }
    }
}

fn join_non_empty(parts: &[&str], separator: &str) -> String {
    let present: Vec<&str> = parts.iter().copied().filter(|p| !p.is_empty()).collect();
    present.join(separator)
}

/// The delimiter that occurs most often in the header line: `,`, `;` or tab (comma wins ties).
pub fn sniff_delimiter(text: &str) -> u8 {
    let header_line = text.lines().next().unwrap_or("");
    let mut best = b',';
    let mut best_count = 0;
    for candidate in *b",;\t" {
        let count = header_line.bytes().filter(|&b| b == candidate).count();
        if count > best_count {
            best = candidate;
            best_count = count;
        }
    }
    best
}

/// Decode the file bytes. UTF-8 (with or without BOM) is the default; other labels such as
/// `cp1252` or `latin1` go through `encoding_rs`.
pub fn decode(bytes: &[u8], encoding: &str) -> Result<String, CliError> {
    let label = encoding.trim().to_lowercase();
    let is_utf8 = label.is_empty() || label == "utf-8" || label == "utf8" || label == "utf-8-sig";
    if is_utf8 {
        let text = String::from_utf8(bytes.to_vec()).map_err(|_| {
            CliError::new("the input is not UTF-8; for old Excel exports try --encoding cp1252")
        })?;
        let without_bom = text.strip_prefix('\u{feff}').unwrap_or(&text);
        return Ok(without_bom.to_string());
    }
    let encoding = encoding_rs::Encoding::for_label(label.as_bytes())
        .ok_or_else(|| CliError::usage(format!("unknown encoding '{encoding}'")))?;
    let (text, _, _) = encoding.decode(bytes);
    Ok(text.into_owned())
}

/// A CSV file in memory: header, rows padded to the header length, and the delimiter used.
pub struct Table {
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub delimiter: u8,
}

/// Parse CSV text; short rows are padded, long rows keep their extra cells.
pub fn read_table(text: &str) -> Result<Table, CliError> {
    let delimiter = sniff_delimiter(text);
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .has_headers(true)
        .from_reader(text.as_bytes());
    let header: Vec<String> = reader
        .headers()
        .map_err(|e| CliError::new(format!("reading CSV header: {e}")))?
        .iter()
        .map(String::from)
        .collect();
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| CliError::new(format!("reading CSV: {e}")))?;
        let mut row: Vec<String> = record.iter().map(String::from).collect();
        if row.len() < header.len() {
            row.resize(header.len(), String::new());
        }
        rows.push(row);
    }
    Ok(Table {
        header,
        rows,
        delimiter,
    })
}

/// How the distinct addresses fared.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub ok: usize,
    pub not_found: usize,
    pub error: usize,
}

/// Where the coordinate and `geo_*` columns live in the output table.
struct OutputColumns {
    lat: usize,
    lon: usize,
    /// Indices of [`GEO_COLUMNS`], in that order.
    geo: Vec<usize>,
}

impl OutputColumns {
    fn status(&self) -> usize {
        self.geo[3]
    }

    fn source(&self) -> usize {
        self.geo[4]
    }
}

/// The output header: the input header plus `lat`/`lon` and the `geo_*` columns where they do
/// not exist yet. Existing coordinate columns (also German names) are reused.
fn output_columns(header: &[String]) -> (Vec<String>, OutputColumns) {
    let mut fieldnames = header.to_vec();
    let mut index_of = |name: &str, existing: Option<usize>| -> usize {
        if let Some(index) = existing {
            return index;
        }
        fieldnames.push(name.to_string());
        fieldnames.len() - 1
    };
    let lat = index_of("lat", find_column(header, &LAT));
    let lon = index_of("lon", find_column(header, &LON));
    let geo: Vec<usize> = GEO_COLUMNS
        .iter()
        .map(|name| index_of(name, header.iter().position(|h| h == name)))
        .collect();
    (fieldnames, OutputColumns { lat, lon, geo })
}

/// Look up every address once, `workers` at a time. Stops early on a fatal error (a rejected
/// Google key), because every further address would fail the same way.
fn lookup_all(addresses: &[String], geocoder: &dyn Geocoder, workers: usize) -> LookupResults {
    let results: Arc<Mutex<LookupResults>> = Arc::new(Mutex::new(HashMap::new()));
    let per_worker = addresses.len().div_ceil(workers).max(1);
    std::thread::scope(|scope| {
        for chunk in addresses.chunks(per_worker) {
            let results = results.clone();
            scope.spawn(move || {
                for address in chunk {
                    let result = geocoder.lookup(address);
                    let fatal = result.as_ref().is_err_and(|e| e.fatal);
                    results.lock().unwrap().insert(address.clone(), result);
                    if fatal {
                        break;
                    }
                }
            });
        }
    });
    Arc::try_unwrap(results)
        .map(|mutex| mutex.into_inner().unwrap())
        .unwrap_or_default()
}

/// Fill coordinates into `table` in place. Returns the output header and the counts.
pub fn geocode_rows(
    table: &mut Table,
    geocoder: &dyn Geocoder,
    columns: &AddressColumns,
    workers: usize,
    force: bool,
) -> Result<(Vec<String>, Counts), CliError> {
    let (fieldnames, out) = output_columns(&table.header);
    for row in table.rows.iter_mut() {
        if row.len() < fieldnames.len() {
            row.resize(fieldnames.len(), String::new());
        }
    }

    // Distinct addresses in first-seen order, each with the rows that share it.
    let mut distinct: Vec<String> = Vec::new();
    let mut rows_by_address: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, row) in table.rows.iter_mut().enumerate() {
        let has_coordinates = !row[out.lat].trim().is_empty() && !row[out.lon].trim().is_empty();
        if has_coordinates && !force {
            row[out.status()] = "existing".to_string();
            continue;
        }
        let address = columns.build(row);
        if address.is_empty() {
            row[out.status()] = "no address".to_string();
            continue;
        }
        if !rows_by_address.contains_key(&address) {
            distinct.push(address.clone());
        }
        rows_by_address.entry(address).or_default().push(index);
    }

    let workers = if geocoder.parallel() {
        workers.max(1)
    } else {
        1
    };
    log(&format!(
        "{} distinct addresses to look up ({}, {workers} parallel) ...",
        distinct.len(),
        geocoder.name()
    ));
    let results = lookup_all(&distinct, geocoder, workers);
    let fatal = results
        .values()
        .filter_map(|r| r.as_ref().err())
        .find(|e| e.fatal);
    if let Some(error) = fatal {
        return Err(CliError::new(error.message.clone()));
    }

    let mut counts = Counts::default();
    for (position, address) in distinct.iter().enumerate() {
        let (hit, error) = match results.get(address) {
            Some(Ok(hit)) => (hit.clone(), String::new()),
            Some(Err(e)) => (None, e.message.clone()),
            None => (None, "no response".to_string()),
        };
        let status = if !error.is_empty() {
            counts.error += 1;
            format!("error: {error}")
        } else if hit.is_some() {
            counts.ok += 1;
            "ok".to_string()
        } else {
            counts.not_found += 1;
            "not found".to_string()
        };
        for &row_index in &rows_by_address[address] {
            let row = &mut table.rows[row_index];
            row[out.status()] = status.clone();
            row[out.source()] = geocoder.name().to_string();
            if let Some(hit) = &hit {
                row[out.lat] = format!("{:.7}", hit.lat);
                row[out.lon] = format!("{:.7}", hit.lon);
                row[out.geo[0]] = hit.address.clone();
                row[out.geo[1]] = hit.accuracy.clone();
                row[out.geo[2]] = hit.partial_match.clone();
            }
        }
        if (position + 1) % PROGRESS_EVERY == 0 {
            log(&format!("  {}/{}", position + 1, distinct.len()));
        }
    }
    Ok((fieldnames, counts))
}

/// Write the table with the delimiter of the input, to a file or to stdout for `-`.
pub fn write_table(
    path: &str,
    fieldnames: &[String],
    rows: &[Vec<String>],
    delimiter: u8,
) -> Result<(), CliError> {
    let mut buffer = Vec::new();
    {
        let mut writer = csv::WriterBuilder::new()
            .delimiter(delimiter)
            .from_writer(&mut buffer);
        writer
            .write_record(fieldnames)
            .map_err(|e| CliError::new(e.to_string()))?;
        for row in rows {
            writer
                .write_record(row)
                .map_err(|e| CliError::new(e.to_string()))?;
        }
        writer.flush().map_err(|e| CliError::new(e.to_string()))?;
    }
    if path == "-" {
        use std::io::Write;
        std::io::stdout()
            .write_all(&buffer)
            .map_err(|e| CliError::new(e.to_string()))
    } else {
        std::fs::write(path, buffer).map_err(|e| CliError::new(format!("writing {path}: {e}")))
    }
}

/// The arguments of `atm-finder geocode`.
pub struct GeocodeOptions<'a> {
    pub input: &'a str,
    pub output: &'a str,
    pub address_columns: &'a [String],
    pub workers: usize,
    pub force: bool,
    pub encoding: &'a str,
}

/// Read, geocode and write; returns the counts for the caller's summary.
pub fn run(opts: &GeocodeOptions, geocoder: &dyn Geocoder) -> Result<Counts, CliError> {
    let bytes = std::fs::read(opts.input).map_err(|e| {
        let cwd = std::env::current_dir()
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        CliError::new(format!("file '{}': {e} (looked in {cwd})", opts.input))
    })?;
    let text = decode(&bytes, opts.encoding)?;
    let mut table = read_table(&text)?;
    let (columns, used) = AddressColumns::detect(&table.header, opts.address_columns)?;
    log(&format!(
        "{} rows, address from: {used}, delimiter {:?}",
        table.rows.len(),
        table.delimiter as char
    ));
    let (fieldnames, counts) =
        geocode_rows(&mut table, geocoder, &columns, opts.workers, opts.force)?;
    write_table(opts.output, &fieldnames, &table.rows, table.delimiter)?;
    log(&format!(
        "Done: {} found, {} not found, {} errors -> {}",
        counts.ok, counts.not_found, counts.error, opts.output
    ));
    if geocoder.name() == "google" {
        log(&format!(
            "{} Google requests (10,000/month free, then USD 5 per 1,000)",
            geocoder.requests()
        ));
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Answers by address content: "Nowhere" is not found, "Broken" fails, the rest is found.
    struct Fake {
        calls: AtomicUsize,
    }

    impl Geocoder for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn parallel(&self) -> bool {
            true
        }
        fn requests(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
        fn lookup(&self, address: &str) -> Result<Option<Hit>, HttpError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if address.contains("Nowhere") {
                return Ok(None);
            }
            if address.contains("Broken") {
                return Err(HttpError::new("boom"));
            }
            Ok(Some(Hit {
                lat: 47.0,
                lon: 15.0,
                address: format!("found {address}"),
                accuracy: "ROOFTOP".into(),
                partial_match: String::new(),
            }))
        }
        fn geocode(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Option<(f64, f64)>, HttpError> {
            Ok(None)
        }
        fn reverse(&self, _: f64, _: f64) -> Result<Option<crate::geocoders::Address>, HttpError> {
            Ok(None)
        }
    }

    #[test]
    fn detects_german_columns_and_keeps_delimiter() {
        let text = "Name;Straße;Hausnummer;PLZ;Ort;lat;lon\nA;Hauptplatz;1;8010;Graz;;\nB;Hauptplatz;1;8010;Graz;;\nC;Nowhere;;;;;\nD;Herrengasse;15;8010;Graz;47.1;15.4\nE;;;;;;\nF;Broken;;;;;\n";
        let mut table = read_table(text).unwrap();
        assert_eq!(table.delimiter, b';');
        let (columns, used) = AddressColumns::detect(&table.header, &[]).unwrap();
        assert_eq!(used, "Straße + Hausnummer + PLZ + Ort");
        assert_eq!(columns.build(&table.rows[0]), "Hauptplatz 1, 8010 Graz");
        let fake = Fake {
            calls: AtomicUsize::new(0),
        };
        let (fieldnames, counts) = geocode_rows(&mut table, &fake, &columns, 4, false).unwrap();
        assert_eq!(fieldnames.len(), 7 + 5);
        assert_eq!(
            counts,
            Counts {
                ok: 1,
                not_found: 1,
                error: 1
            }
        );
        assert_eq!(fake.requests(), 3, "identical addresses are looked up once");
        let status_i = fieldnames.iter().position(|f| f == "geo_status").unwrap();
        let status = |i: usize| table.rows[i][status_i].clone();
        assert_eq!(
            (status(0).as_str(), status(1).as_str(), status(2).as_str()),
            ("ok", "ok", "not found")
        );
        assert_eq!(
            (status(3).as_str(), status(4).as_str(), status(5).as_str()),
            ("existing", "no address", "error: boom")
        );
        assert_eq!(table.rows[0][5], "47.0000000");
        let mut buf = Vec::new();
        {
            let mut w = csv::WriterBuilder::new()
                .delimiter(table.delimiter)
                .from_writer(&mut buf);
            w.write_record(&fieldnames).unwrap();
        }
        assert!(String::from_utf8(buf)
            .unwrap()
            .starts_with("Name;Straße;Hausnummer;PLZ;Ort;lat;lon;geo_address"));
    }

    #[test]
    fn single_and_explicit_columns() {
        let header: Vec<String> = ["Adresse", "x"].iter().map(|s| s.to_string()).collect();
        let (c, used) = AddressColumns::detect(&header, &[]).unwrap();
        assert_eq!(used, "Adresse");
        assert_eq!(
            c.build(&["Hauptplatz 1".into(), "y".into()]),
            "Hauptplatz 1"
        );
        let (c, _) =
            AddressColumns::detect(&header, &["x".to_string(), "Adresse".to_string()]).unwrap();
        assert_eq!(
            c.build(&["Hauptplatz 1".into(), "y".into()]),
            "y, Hauptplatz 1"
        );
        assert!(AddressColumns::detect(&header, &["nope".to_string()]).is_err());
        assert!(AddressColumns::detect(&["a".to_string()], &[]).is_err());
    }

    #[test]
    fn decoding_and_sniffing() {
        assert_eq!(decode("\u{feff}a,b".as_bytes(), "utf-8").unwrap(), "a,b");
        assert_eq!(decode(&[b'G', b'r', 0xE4, b'n'], "cp1252").unwrap(), "Grän");
        assert_eq!(sniff_delimiter("a\tb\tc\n"), b'\t');
        assert_eq!(sniff_delimiter("a,b;c,d\n"), b',');
        assert_eq!(sniff_delimiter("plain\n"), b',');
    }

    #[test]
    fn output_columns_reuse_existing_coordinates() {
        let header: Vec<String> = ["Name", "Breite", "Länge", "geo_status"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (fieldnames, out) = output_columns(&header);
        assert_eq!((out.lat, out.lon, out.status()), (1, 2, 3));
        assert_eq!(fieldnames.len(), 4 + 4);
    }
}
