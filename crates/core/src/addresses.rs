//! Address completion helpers.
//!
//! Roughly half of the ATMs in OpenStreetMap carry no address. The best substitute is the
//! nearest object *with* a house number: one Overpass query asks for everything tagged
//! `addr:housenumber` within a small radius of each ATM (`around:` filter), and
//! [`match_nearest`] picks the closest per ATM. Real street addresses win over
//! `addr:place` entries such as market stalls, even when the stall is closer.
//!
//! ATMs that have no house number nearby fall back to reverse geocoding with Nominatim;
//! [`parse_nominatim_reverse`] turns such a response into street, postcode and city. The
//! requests themselves are made by the command-line tool and the website.

use crate::geo::haversine_m;

/// The address found next to an ATM.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// Street (or place) and house number.
    pub street: String,
    pub postcode: String,
    pub city: String,
    /// How far the address object is from the ATM.
    pub distance_m: f64,
}

/// ATMs per Overpass query. Each ATM adds one `around:` clause; more than this makes the query
/// slow and, on a busy server, likely to time out.
pub const BATCH_SIZE: usize = 150;

/// Overpass query for all objects with a house number within `radius_m` metres of each point
/// (`(lat, lon)`), with `out center` so that buildings get a position.
pub fn nearby_query(points: &[(f64, f64)], radius_m: f64) -> String {
    let mut clauses = Vec::new();
    for (lat, lon) in points {
        clauses.push(format!(
            "  nwr[\"addr:housenumber\"](around:{radius_m:.0},{lat:.7},{lon:.7});"
        ));
    }
    format!(
        "[out:json][timeout:180];\n(\n{}\n);\nout center tags;",
        clauses.join("\n")
    )
}

/// Text of a JSON field, empty when missing or not a string.
fn text<'a>(value: &'a serde_json::Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(|field| field.as_str())
        .unwrap_or("")
}

/// An address object from the nearby query.
struct AddressCandidate {
    lat: f64,
    lon: f64,
    street: String,
    postcode: String,
    city: String,
    /// A proper street address (`addr:street` plus a number that starts with a digit), as
    /// opposed to `addr:place` or a stall number like "Stand 12".
    is_street_address: bool,
}

/// Address objects from the response to [`nearby_query`]. Objects without a position, a
/// street/place name or a house number are useless and skipped.
fn collect_address_candidates(data: &serde_json::Value) -> Vec<AddressCandidate> {
    let mut candidates = Vec::new();
    let elements = data
        .get("elements")
        .and_then(|elements| elements.as_array());
    for element in elements.into_iter().flatten() {
        let Some((lat, lon)) = crate::model::element_position(element) else {
            continue;
        };
        let no_tags = serde_json::Value::Null;
        let tags = element.get("tags").unwrap_or(&no_tags);

        let street_tag = text(tags, "addr:street");
        let street_name = if street_tag.is_empty() {
            text(tags, "addr:place")
        } else {
            street_tag
        };
        let house_number = text(tags, "addr:housenumber");
        if street_name.is_empty() || house_number.is_empty() {
            continue;
        }
        let number_starts_with_digit = house_number
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit());

        candidates.push(AddressCandidate {
            lat,
            lon,
            street: format!("{street_name} {house_number}"),
            postcode: text(tags, "addr:postcode").to_string(),
            city: text(tags, "addr:city").to_string(),
            is_street_address: !street_tag.is_empty() && number_starts_with_digit,
        });
    }
    candidates
}

/// The best candidate within `radius_m` of a point: street addresses before other kinds, then
/// the closest. `None` when nothing is within the radius.
fn closest_candidate(
    lat: f64,
    lon: f64,
    candidates: &[AddressCandidate],
    radius_m: f64,
) -> Option<Hit> {
    let mut best: Option<(&AddressCandidate, f64)> = None;
    for candidate in candidates {
        let distance = haversine_m(lat, lon, candidate.lat, candidate.lon);
        if distance > radius_m {
            continue;
        }
        let is_better = match best {
            None => true,
            Some((current, current_distance)) => {
                // Tuples compare element by element: a street address (false) beats a
                // non-street address (true), and among equals the shorter distance wins.
                (!candidate.is_street_address, distance)
                    < (!current.is_street_address, current_distance)
            }
        };
        if is_better {
            best = Some((candidate, distance));
        }
    }
    best.map(|(candidate, distance)| Hit {
        street: candidate.street.clone(),
        postcode: candidate.postcode.clone(),
        city: candidate.city.clone(),
        distance_m: distance,
    })
}

/// For each point, the closest address object within `radius_m` from the response to
/// [`nearby_query`]; real street addresses beat places and stalls.
pub fn match_nearest(
    points: &[(f64, f64)],
    data: &serde_json::Value,
    radius_m: f64,
) -> Vec<Option<Hit>> {
    let candidates = collect_address_candidates(data);
    points
        .iter()
        .map(|&(lat, lon)| closest_candidate(lat, lon, &candidates, radius_m))
        .collect()
}

/// `(street + number, postcode, city)` from a Nominatim `reverse` response, or `None` when the
/// response has no road-like component (then it is not a usable address).
pub fn parse_nominatim_reverse(data: &serde_json::Value) -> Option<(String, String, String)> {
    let address = data.get("address")?;
    let road = ["road", "pedestrian", "square", "footway"]
        .iter()
        .map(|key| text(address, key))
        .find(|value| !value.is_empty())?;
    let house_number = text(address, "house_number");
    let street = if house_number.is_empty() {
        road.to_string()
    } else {
        format!("{road} {house_number}")
    };
    let city = ["city", "town", "village", "municipality"]
        .iter()
        .map(|key| text(address, key))
        .find(|value| !value.is_empty())
        .unwrap_or("");
    Some((
        street,
        text(address, "postcode").to_string(),
        city.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: (f64, f64) = (47.07, 15.44);

    fn el(dist_east_m: f64, tags: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"type": "node", "id": 1, "lat": P.0, "lon": P.1 + dist_east_m / 75_800.0, "tags": tags})
    }

    #[test]
    fn prefers_street_address_over_stall_and_respects_radius() {
        let data = serde_json::json!({"elements": [
            el(5.0, serde_json::json!({"addr:place": "Bauernmarkt", "addr:housenumber": "Stand 12"})),
            el(30.0, serde_json::json!({"addr:street": "Hauptplatz", "addr:housenumber": "3", "addr:postcode": "8010", "addr:city": "Graz"})),
            el(80.0, serde_json::json!({"addr:street": "Nah", "addr:housenumber": "1"})),
        ]});
        let hits = match_nearest(&[P, (47.2, 15.6)], &data, 50.0);
        let hit = hits[0].as_ref().unwrap();
        assert_eq!(
            (
                hit.street.as_str(),
                hit.postcode.as_str(),
                hit.city.as_str()
            ),
            ("Hauptplatz 3", "8010", "Graz")
        );
        assert!((hit.distance_m - 30.0).abs() < 2.0);
        assert!(hits[1].is_none());
    }

    #[test]
    fn nearby_query_and_reverse_parsing() {
        let q = nearby_query(&[P, (47.1, 15.5)], 50.0);
        assert_eq!(q.matches("around:50,").count(), 2);
        let data = serde_json::json!({"address": {"road": "Annenstraße", "house_number": "5", "postcode": "8020", "town": "Graz"}});
        assert_eq!(
            parse_nominatim_reverse(&data),
            Some(("Annenstraße 5".into(), "8020".into(), "Graz".into()))
        );
        assert_eq!(
            parse_nominatim_reverse(&serde_json::json!({"address": {"postcode": "1"}})),
            None
        );
    }
}
