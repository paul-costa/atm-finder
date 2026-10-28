//! Web listings via Firecrawl: the extraction schema, the prompt, and the parsing of what the
//! LLM extracted.
//!
//! Firecrawl searches the web, scrapes the pages and asks a language model to fill the JSON
//! schema below from each page. Such listings rarely contain coordinates, so the records come
//! back as addresses that the caller geocodes; coordinates are only trusted when the page states
//! them literally. The HTTP requests and the geocoding live in the command-line tool.

/// Base URL of the Firecrawl API.
pub const API_URL: &str = "https://api.firecrawl.dev/v2";

/// JSON schema the LLM extraction has to fill: a list of ATMs with address and operator, and
/// coordinates only when printed on the page.
pub fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "atms": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "operator": {"type": "string", "description": "Bank or operator of the machine"},
                        "street": {"type": "string", "description": "Street and house number"},
                        "postcode": {"type": "string"},
                        "city": {"type": "string"},
                        "latitude": {"type": "number", "description": "Only if stated literally on the page"},
                        "longitude": {"type": "number", "description": "Only if stated literally on the page"}
                    },
                    "required": ["street"]
                }
            }
        },
        "required": ["atms"]
    })
}

/// Extraction prompt for one place; it tells the model not to invent anything.
pub fn prompt(place: &str) -> String {
    format!(
        "List every ATM (Bankomat, Geldautomat) in {place} mentioned on this page, with address and operator. \
         Do not invent anything; give coordinates only when they are written on the page."
    )
}

/// Web searches to run when the user gives none: German, because the listings are.
pub fn default_queries(place: &str) -> Vec<String> {
    vec![
        format!("Bankomat {place} Standorte Adresse"),
        format!("Geldautomat {place} Liste"),
    ]
}

/// One ATM as extracted from a web page.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub name: String,
    pub operator: String,
    pub street: String,
    pub postcode: String,
    pub city: String,
    /// Only set when the page stated the coordinates.
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    /// The page the record was extracted from.
    pub page_url: String,
    /// Position within that page's list, to build a source id.
    pub index: usize,
}

/// Text of a field; numbers are turned into text too, because models sometimes return a
/// postcode as a number.
fn field_text(record: &serde_json::Value, key: &str) -> String {
    match record.get(key) {
        Some(serde_json::Value::String(text)) => text.trim().to_string(),
        Some(serde_json::Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

/// Records from the extracted pages, given as `(page url, extracted JSON)` pairs.
///
/// Records without a street are useless and dropped; the same street and postcode seen twice
/// (case-insensitively) is kept once. A missing city defaults to `default_city`.
pub fn extract(pages: &[(String, serde_json::Value)], default_city: &str) -> Vec<Record> {
    let mut seen = std::collections::HashSet::new();
    let mut records = Vec::new();
    for (page_url, extracted) in pages {
        let atms = extracted.get("atms").and_then(|atms| atms.as_array());
        for (index, record) in atms.into_iter().flatten().enumerate() {
            let street = field_text(record, "street");
            if street.is_empty() {
                continue;
            }
            let postcode = field_text(record, "postcode");
            let is_new = seen.insert((street.to_lowercase(), postcode.clone()));
            if !is_new {
                continue;
            }
            let mut city = field_text(record, "city");
            if city.is_empty() {
                city = default_city.to_string();
            }
            records.push(Record {
                name: field_text(record, "name"),
                operator: field_text(record, "operator"),
                street,
                postcode,
                city,
                lat: record.get("latitude").and_then(|value| value.as_f64()),
                lon: record.get("longitude").and_then(|value| value.as_f64()),
                page_url: page_url.clone(),
                index,
            });
        }
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_dedupes_and_defaults_city() {
        let pages = vec![
            (
                "https://a".to_string(),
                serde_json::json!({"atms": [
                    {"street": "Hauptplatz 1", "postcode": "8010", "operator": "Sparkasse", "latitude": 47.07, "longitude": 15.44},
                    {"street": "hauptplatz 1", "postcode": "8010"},
                    {"street": "", "postcode": "8010"}
                ]}),
            ),
            (
                "https://b".to_string(),
                serde_json::json!({"atms": [{"street": "Annenstraße 5", "city": "Graz"}]}),
            ),
        ];
        let recs = extract(&pages, "Graz");
        assert_eq!(recs.len(), 2);
        assert_eq!(
            (recs[0].city.as_str(), recs[0].lat, recs[0].index),
            ("Graz", Some(47.07), 0)
        );
        assert_eq!(
            (recs[1].page_url.as_str(), recs[1].postcode.as_str()),
            ("https://b", "")
        );
        assert!(schema()["required"][0] == "atms");
    }
}
