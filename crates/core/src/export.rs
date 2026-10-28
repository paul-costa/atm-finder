//! The three output formats: CSV for spreadsheets, GeoJSON for mapping software and KML for
//! Google My Maps.
//!
//! All formats carry the same [`COLUMNS`]. CSV follows RFC 4180 (comma separated, cells quoted
//! only when needed). GeoJSON points use `[lon, lat]`, as the standard requires, and repeat the
//! columns as properties. KML puts every column into `ExtendedData`, which Google My Maps shows
//! in a placemark's details.

use crate::boundary::Boundary;
use crate::model::Atm;

/// Columns of every export, in order. See the README for what each one holds.
pub const COLUMNS: [&str; 21] = [
    "id",
    "lat",
    "lon",
    "name",
    "operator",
    "brand",
    "street",
    "postcode",
    "city",
    "address_source",
    "kind",
    "opening_hours",
    "fee",
    "cash_in",
    "wheelchair",
    "indoor",
    "check_date",
    "place",
    "sources",
    "source_ids",
    "url",
];

/// Sort key of a row: place, postcode (unknown last), street, position. Keeps exports stable
/// between runs so that diffs stay small.
fn sort_key(atm: &Atm) -> (&str, &str, String) {
    let postcode = if atm.postcode.is_empty() {
        "9999"
    } else {
        &atm.postcode
    };
    (&atm.place, postcode, atm.street.to_lowercase())
}

/// Order rows by place, postcode, street and latitude.
pub fn sort_rows(rows: &mut [Atm]) {
    rows.sort_by(|a, b| {
        let by_address = sort_key(a).cmp(&sort_key(b));
        let by_latitude = a
            .lat
            .partial_cmp(&b.lat)
            .unwrap_or(std::cmp::Ordering::Equal);
        by_address.then(by_latitude)
    });
}

/// The cell values of one ATM, in [`COLUMNS`] order.
pub fn row(atm: &Atm) -> Vec<String> {
    let members = atm.members();

    let mut sources: Vec<&str> = members
        .iter()
        .map(|member| member.source.as_str())
        .collect();
    sources.sort();
    sources.dedup();

    let source_ids: Vec<String> = members.iter().map(|member| member.stable_id()).collect();

    vec![
        atm.stable_id(),
        format!("{:.7}", atm.lat),
        format!("{:.7}", atm.lon),
        atm.name.clone(),
        atm.operator.clone(),
        atm.brand.clone(),
        atm.street.clone(),
        atm.postcode.clone(),
        atm.city.clone(),
        atm.address_source_label().to_string(),
        atm.kind.clone(),
        atm.opening_hours.clone(),
        atm.fee.clone(),
        atm.cash_in.clone(),
        atm.wheelchair.clone(),
        atm.indoor.clone(),
        atm.check_date.clone(),
        atm.place.clone(),
        sources.join(";"),
        source_ids.join(";"),
        atm.url.clone(),
    ]
}

/// A CSV cell: quoted (with inner quotes doubled) when it contains a comma, quote or line break.
fn csv_cell(value: &str) -> String {
    let needs_quotes =
        value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r');
    if needs_quotes {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// The rows as CSV text with a header line.
pub fn to_csv(rows: &[Atm]) -> String {
    let mut csv = COLUMNS.join(",");
    csv.push('\n');
    for atm in rows {
        let cells: Vec<String> = row(atm).iter().map(|value| csv_cell(value)).collect();
        csv.push_str(&cells.join(","));
        csv.push('\n');
    }
    csv
}

/// Round to seven decimals (about a centimetre), the precision the CSV uses as well.
fn round7(value: f64) -> f64 {
    (value * 1e7).round() / 1e7
}

/// The rows as a GeoJSON `FeatureCollection` of points, with the columns as properties
/// (plus numeric `lat`/`lon`). The boundary, when given, is appended as a polygon feature.
pub fn to_geojson_value(rows: &[Atm], boundary: Option<&Boundary>) -> serde_json::Value {
    let mut features: Vec<serde_json::Value> = Vec::new();
    for atm in rows {
        let mut properties = serde_json::Map::new();
        for (column, value) in COLUMNS.iter().zip(row(atm)) {
            properties.insert((*column).to_string(), serde_json::Value::String(value));
        }
        properties.insert("lat".into(), serde_json::json!(atm.lat));
        properties.insert("lon".into(), serde_json::json!(atm.lon));
        features.push(serde_json::json!({
            "type": "Feature",
            "geometry": {"type": "Point", "coordinates": [round7(atm.lon), round7(atm.lat)]},
            "properties": properties,
        }));
    }
    if let Some(boundary) = boundary {
        features.push(boundary.to_feature());
    }
    serde_json::json!({"type": "FeatureCollection", "features": features})
}

/// [`to_geojson_value`] serialised to text.
pub fn to_geojson(rows: &[Atm], boundary: Option<&Boundary>) -> String {
    serde_json::to_string(&to_geojson_value(rows, boundary)).unwrap_or_default()
}

/// Escape text for an XML element or attribute.
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The placemark title: operator, brand or name, whichever is known first.
fn kml_label(atm: &Atm) -> &str {
    [&atm.operator, &atm.brand, &atm.name]
        .into_iter()
        .find(|value| !value.is_empty())
        .map(String::as_str)
        .unwrap_or("ATM")
}

/// The placemark description: address, opening hours and source link, one per line.
fn kml_description(atm: &Atm) -> String {
    let mut lines = vec![atm.address_line()];
    if !atm.opening_hours.is_empty() {
        lines.push(format!("Opening hours: {}", atm.opening_hours));
    }
    if !atm.url.is_empty() {
        lines.push(atm.url.clone());
    }
    lines.join("\n")
}

/// The rows as KML with one placemark per ATM and every column as `ExtendedData`.
pub fn to_kml(rows: &[Atm], document_name: &str) -> String {
    let mut kml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<kml xmlns=\"http://www.opengis.net/kml/2.2\">\n<Document>\n",
    );
    kml.push_str(&format!("<name>{}</name>\n", xml(document_name)));
    kml.push_str(
        "<Style id=\"atm\"><IconStyle><scale>1.0</scale><Icon><href>https://maps.google.com/mapfiles/kml/shapes/dollar.png</href></Icon></IconStyle></Style>\n",
    );
    for atm in rows {
        kml.push_str("<Placemark>\n");
        kml.push_str(&format!("<name>{}</name>\n", xml(kml_label(atm))));
        kml.push_str(&format!(
            "<description>{}</description>\n",
            xml(&kml_description(atm))
        ));
        kml.push_str("<styleUrl>#atm</styleUrl>\n<ExtendedData>\n");
        for (column, value) in COLUMNS.iter().zip(row(atm)) {
            kml.push_str(&format!(
                "<Data name=\"{column}\"><value>{}</value></Data>\n",
                xml(&value)
            ));
        }
        kml.push_str("</ExtendedData>\n");
        kml.push_str(&format!(
            "<Point><coordinates>{:.7},{:.7},0</coordinates></Point>\n",
            atm.lon, atm.lat
        ));
        kml.push_str("</Placemark>\n");
    }
    kml.push_str("</Document>\n</kml>\n");
    kml
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Atm> {
        let mut a = Atm {
            lat: 47.0707,
            lon: 15.4395,
            operator: "Sparkasse".into(),
            street: "Hauptplatz 1, Ecke".into(),
            postcode: "8010".into(),
            city: "Graz".into(),
            kind: "atm".into(),
            source: "osm".into(),
            source_id: "node/1".into(),
            url: "https://www.openstreetmap.org/node/1".into(),
            place: "Graz".into(),
            ..Default::default()
        };
        a.merged.push(Atm {
            source: "osm".into(),
            source_id: "node/2".into(),
            ..Default::default()
        });
        let b = Atm {
            lat: 47.0,
            lon: 15.0,
            name: "A & B <Bank>".into(),
            kind: "bank_with_atm".into(),
            source: "osm".into(),
            source_id: "way/3".into(),
            place: "Graz".into(),
            ..Default::default()
        };
        vec![b, a]
    }

    #[test]
    fn csv_has_header_quoting_and_merged_ids() {
        let mut rows = sample();
        sort_rows(&mut rows);
        let csv = to_csv(&rows);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], COLUMNS.join(","));
        assert!(lines[1].starts_with("osm:node/1,47.0707000,15.4395000,,Sparkasse,,\"Hauptplatz 1, Ecke\",8010,Graz,source,atm"));
        assert!(lines[1].contains("osm,osm:node/1;osm:node/2,"));
        assert!(lines[2].starts_with("osm:way/3,"));
    }

    #[test]
    fn geojson_and_kml() {
        let rows = sample();
        let gj = to_geojson_value(&rows, None);
        assert_eq!(gj["features"].as_array().unwrap().len(), 2);
        assert_eq!(
            gj["features"][1]["geometry"]["coordinates"],
            serde_json::json!([15.4395, 47.0707])
        );
        assert_eq!(gj["features"][1]["properties"]["id"], "osm:node/1");
        let kml = to_kml(&rows, "ATMs Graz");
        assert_eq!(kml.matches("<Placemark>").count(), 2);
        assert!(
            kml.contains("A &amp; B &lt;Bank&gt;")
                && kml.contains("<Data name=\"id\"><value>osm:node/1</value></Data>")
        );
        assert!(kml.contains("<description>Hauptplatz 1, Ecke, 8010 Graz\nhttps://www.openstreetmap.org/node/1</description>"));
    }
}
