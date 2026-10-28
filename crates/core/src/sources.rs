//! Data sources besides OpenStreetMap.
//!
//! OpenStreetMap itself is split between `boundary::atms_query` (the query) and
//! `model::parse_overpass_atms` (the parsing). This module holds the other sources' parsers;
//! the requests are made by the command-line tool.

/// Names of all sources, as accepted by `--sources`.
pub const SOURCES: [&str; 3] = ["osm", "bankaustria", "firecrawl"];
/// What runs when `--sources` is not given.
pub const DEFAULT_SOURCES: &str = "osm";

/// Branches with cash withdrawal from the UniCredit Bank Austria branch finder.
///
/// The branch finder has an open JSON endpoint that returns every branch in Austria in one
/// response, with "advanced filters" describing the equipment and opening hours of the
/// self-service area. Callers clip the result to the boundary.
pub mod bankaustria {
    use crate::model::Atm;

    /// The branch finder endpoint; one request returns all branches.
    pub const URL: &str =
        "https://www.bankaustria.at/filialen/api/storefinder-locations.jsp?language=de";
    /// Day keys as the endpoint uses them, in the order OSM opening hours list them.
    const DAYS: [&str; 7] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

    /// Text of a field, empty when missing or not a string.
    fn text<'a>(value: &'a serde_json::Value, key: &str) -> &'a str {
        value
            .get(key)
            .and_then(|field| field.as_str())
            .unwrap_or("")
    }

    /// Text of a field that may be a string or a number (ids and postcodes come as either).
    fn text_or_number(value: &serde_json::Value, key: &str) -> String {
        match value.get(key) {
            Some(serde_json::Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        }
    }

    /// A number that may come as a JSON number or as a string.
    fn number(value: &serde_json::Value) -> Option<f64> {
        if let Some(number) = value.as_f64() {
            return Some(number);
        }
        value.as_str()?.parse().ok()
    }

    /// The values of the advanced filter called `name`, e.g. the equipment list.
    fn filter_values(location: &serde_json::Value, name: &str) -> Vec<String> {
        let mut values = Vec::new();
        let filters = location
            .get("advancedFilters")
            .and_then(|filters| filters.as_array());
        for filter in filters.into_iter().flatten() {
            if text(filter, "name") != name {
                continue;
            }
            let filter_values = filter.get("values").and_then(|values| values.as_array());
            for value in filter_values.into_iter().flatten() {
                if let Some(value) = value.as_str() {
                    values.push(value.to_string());
                }
            }
        }
        values
    }

    /// Self-service opening hours (`Mo=08:00-20:00`, ...) in OSM style, `24/7` when the
    /// machines are reachable around the clock on every day.
    fn opening_hours_text(location: &serde_json::Value) -> String {
        let entries = filter_values(location, "openingHoursSB");
        let mut per_day: Vec<String> = Vec::new();
        for day in DAYS {
            let hours = entries.iter().find_map(|entry| {
                let (entry_day, hours) = entry.split_once('=')?;
                if entry_day == day {
                    Some(hours.to_string())
                } else {
                    None
                }
            });
            if let Some(hours) = hours {
                per_day.push(format!("{day} {hours}"));
            }
        }
        let open_all_week =
            per_day.len() == 7 && per_day.iter().all(|entry| entry.ends_with(" 00:00-24:00"));
        if open_all_week {
            "24/7".to_string()
        } else {
            per_day.join("; ")
        }
    }

    /// Every branch in Austria that offers cash withdrawal, as records; callers clip them to
    /// the boundary. Branches without usable coordinates are skipped.
    pub fn parse_locations(data: &serde_json::Value, place: &str) -> Vec<Atm> {
        let mut records = Vec::new();
        let locations = data
            .get("response")
            .and_then(|response| response.get("locations"))
            .and_then(|locations| locations.as_array());
        for location in locations.into_iter().flatten() {
            let equipment = filter_values(location, "filialausstattungFilter");
            let has_cash_withdrawal = equipment.iter().any(|item| item == "Bargeld beheben");
            if !has_cash_withdrawal {
                continue;
            }
            let lat = location.get("lat").and_then(number);
            let lon = location.get("lng").and_then(number);
            let (Some(lat), Some(lon)) = (lat, lon) else {
                continue;
            };
            records.push(Atm {
                lat,
                lon,
                name: text(location, "name").to_string(),
                operator: "UniCredit Bank Austria".into(),
                brand: "Bank Austria".into(),
                street: text(location, "streetAndNumber").to_string(),
                postcode: text_or_number(location, "zip"),
                city: text(location, "city").to_string(),
                kind: "branch_with_atm".into(),
                opening_hours: opening_hours_text(location),
                source: "bankaustria".into(),
                source_id: text_or_number(location, "id"),
                url: "https://www.bankaustria.at/filialen/".into(),
                place: place.to_string(),
                ..Default::default()
            });
        }
        records
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn keeps_cash_branches_and_builds_hours() {
            let data = serde_json::json!({"response": {"locations": [
                {"id": 1, "name": "Graz Herrengasse", "lat": "47.07", "lng": 15.44, "streetAndNumber": "Herrengasse 15", "zip": "8010", "city": "Graz",
                 "advancedFilters": [{"name": "filialausstattungFilter", "values": ["Bargeld beheben"]},
                                     {"name": "openingHoursSB", "values": ["Mo=00:00-24:00", "Tu=00:00-24:00", "We=00:00-24:00", "Th=00:00-24:00", "Fr=00:00-24:00", "Sa=00:00-24:00", "Su=00:00-24:00"]}]},
                {"id": 2, "name": "Beratung", "lat": 47.0, "lng": 15.0, "advancedFilters": [{"name": "filialausstattungFilter", "values": ["Beratung"]}]},
                {"id": 3, "name": "Kaputt", "lat": "x", "lng": 15.0, "advancedFilters": [{"name": "filialausstattungFilter", "values": ["Bargeld beheben"]},
                                     {"name": "openingHoursSB", "values": ["Mo=08:00-12:00"]}]}
            ]}});
            let out = parse_locations(&data, "Graz");
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].opening_hours, "24/7");
            assert_eq!(
                (
                    out[0].postcode.as_str(),
                    out[0].source_id.as_str(),
                    out[0].kind.as_str()
                ),
                ("8010", "1", "branch_with_atm")
            );
        }

        #[test]
        fn partial_hours_are_listed_per_day() {
            let data = serde_json::json!({"response": {"locations": [
                {"id": "x", "lat": 47.0, "lng": 15.0, "advancedFilters": [
                    {"name": "filialausstattungFilter", "values": ["Bargeld beheben"]},
                    {"name": "openingHoursSB", "values": ["Tu=08:00-12:00", "Mo=08:00-20:00", "broken"]}]}
            ]}});
            let out = parse_locations(&data, "Graz");
            assert_eq!(out[0].opening_hours, "Mo 08:00-20:00; Tu 08:00-12:00");
            assert_eq!(out[0].source_id, "x");
        }
    }
}
