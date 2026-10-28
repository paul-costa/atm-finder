//! The ATM record, operator normalisation and the merging of duplicate records.
//!
//! One `Atm` is one row of the export. It starts life as a *record* from a source (an
//! OpenStreetMap element, a bank-branch entry, a web listing); [`merge`] then folds records that
//! describe the same machine into one ATM, keeping the most precise record as the head and the
//! others in `merged`.
//!
//! OpenStreetMap maps ATMs in three ways, which is why the ATM query asks for all of them:
//! a separately mapped machine (`amenity=atm`), a bank branch that has one (`amenity=bank` +
//! `atm=yes`), and some other place with a machine inside, such as a supermarket (`atm=yes` on
//! any other amenity). The Overpass query uses `out center`, so ways and relations carry a
//! `center` instead of `lat`/`lon`.

use crate::geo::{haversine_m, GridIndex};

/// One ATM, or one raw record before merging. All text fields are empty when unknown.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Atm {
    pub lat: f64,
    pub lon: f64,
    /// Name of the machine or of the branch/shop it belongs to.
    pub name: String,
    /// Bank that runs the machine, as tagged (`operator`, falling back to `brand`).
    pub operator: String,
    /// Brand as tagged, kept separately because it can differ from the operator.
    pub brand: String,
    /// Street and house number, e.g. `Hauptplatz 1`.
    pub street: String,
    pub postcode: String,
    pub city: String,
    /// How the record was mapped: `atm`, `bank_with_atm`, `poi_with_atm`, `branch_with_atm`
    /// (bank branch finder) or `web_listing` (Firecrawl). Decides the priority when merging.
    pub kind: String,
    /// Opening hours in OSM syntax, or `24/7`.
    pub opening_hours: String,
    pub fee: String,
    pub cash_in: String,
    pub wheelchair: String,
    pub indoor: String,
    /// Date of the last survey (`check_date` or `survey:date`).
    pub check_date: String,
    /// Where the record came from: `osm`, `bankaustria` or `firecrawl`.
    pub source: String,
    /// Identifier within the source, e.g. `node/123` for OSM.
    pub source_id: String,
    /// Link to the record at its source.
    pub url: String,
    /// How the address was obtained: empty when it came with the record, `osm_nearby` for the
    /// nearest house number, `nominatim_reverse` / `google_reverse` for reverse geocoding.
    pub address_source: String,
    /// The place the row was exported for, so that rows of several places can be told apart.
    pub place: String,
    /// Records that were merged into this one.
    pub merged: Vec<Atm>,
}

impl Atm {
    /// Identifier that survives re-runs, e.g. `osm:node/123`; empty for a record without an id.
    pub fn stable_id(&self) -> String {
        if self.source_id.is_empty() {
            String::new()
        } else {
            format!("{}:{}", self.source, self.source_id)
        }
    }

    /// This record followed by everything merged into it.
    pub fn members(&self) -> Vec<&Atm> {
        let mut members = vec![self];
        members.extend(self.merged.iter());
        members
    }

    /// Value of the `address_source` column: the recorded method, `source` when the address
    /// came with the record, empty when there is no address at all.
    pub fn address_source_label(&self) -> &str {
        if !self.address_source.is_empty() {
            &self.address_source
        } else if !self.street.is_empty() {
            "source"
        } else {
            ""
        }
    }

    /// `Street 1, 8010 Graz`, with whichever parts are known; empty without a street.
    pub fn address_line(&self) -> String {
        if self.street.is_empty() {
            return String::new();
        }
        let town = join_non_empty(&[&self.postcode, &self.city], " ");
        join_non_empty(&[&self.street, &town], ", ")
    }
}

/// Join the non-empty strings with `separator`.
fn join_non_empty(parts: &[&str], separator: &str) -> String {
    let non_empty: Vec<&str> = parts
        .iter()
        .copied()
        .filter(|part| !part.is_empty())
        .collect();
    non_empty.join(separator)
}

/// Priority of a record kind when merging: lower wins. A separately mapped machine gives the
/// most precise position, so branches and web listings attach to it rather than the reverse.
pub fn kind_priority(kind: &str) -> u8 {
    match kind {
        "atm" => 0,
        "branch_with_atm" => 1,
        "bank_with_atm" => 2,
        "poi_with_atm" => 3,
        "web_listing" => 4,
        _ => 9,
    }
}

/// Known Austrian operators: the shared key and the lower-case fragments that identify them.
/// "Die Steiermärkische" and "Steiermärkische Sparkasse" are the same bank, and so on.
const OPERATOR_KEYS: &[(&str, &[&str])] = &[
    (
        "sparkasse",
        &[
            "sparkasse",
            "steiermärkische",
            "steiermaerkische",
            "erste bank",
            "erste group",
        ],
    ),
    ("raiffeisen", &["raiffeisen", "rlb", "raika", "rb "]),
    ("bawag", &["bawag", "psk", "p.s.k", "easybank"]),
    ("bankaustria", &["bank austria", "unicredit"]),
    ("volksbank", &["volksbank"]),
    ("hypo", &["hypo"]),
    (
        "bank99",
        &["bank99", "bank 99", "post ag", "österreichische post"],
    ),
    ("euronet", &["euronet"]),
    ("oberbank", &["oberbank"]),
    ("bks", &["bks"]),
    ("btv", &["btv", "bank für tirol"]),
    ("dadat", &["dadat", "schelhammer"]),
    ("spar", &["spar ", "spar-", "eurospar", "interspar"]),
];

/// Normalised key for an operator name, so that differently written names of the same bank
/// compare equal. Unknown names are reduced to their lower-case alphanumerics; an empty name
/// gives an empty key, which [`merge`] treats as "operator unknown".
pub fn operator_key(name: &str) -> String {
    let lower = name.to_lowercase();
    for (key, fragments) in OPERATOR_KEYS {
        let matches = fragments
            .iter()
            .any(|fragment| lower.contains(fragment) || lower == fragment.trim());
        if matches {
            return (*key).to_string();
        }
    }
    lower
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// Text of an OSM tag, empty when the tag is missing.
fn tag_text(tags: &serde_json::Value, name: &str) -> String {
    tags.get(name)
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string()
}

/// Position of an Overpass element: `lat`/`lon` of a node, or the `center` that
/// `out center` adds to ways and relations.
pub(crate) fn element_position(element: &serde_json::Value) -> Option<(f64, f64)> {
    let center = element.get("center");
    let lat = element
        .get("lat")
        .or_else(|| center?.get("lat"))?
        .as_f64()?;
    let lon = element
        .get("lon")
        .or_else(|| center?.get("lon"))?
        .as_f64()?;
    Some((lat, lon))
}

/// Turn the elements of the ATM query (`out center tags`) into records.
///
/// Elements without a position are skipped. The address is built from `addr:street` (or
/// `addr:place` for places without streets) and `addr:housenumber`; the operator falls back to
/// the brand; the survey date comes from `check_date` or the older `survey:date`.
pub fn parse_overpass_atms(data: &serde_json::Value, place: &str) -> Vec<Atm> {
    let mut records = Vec::new();
    let Some(elements) = data
        .get("elements")
        .and_then(|elements| elements.as_array())
    else {
        return records;
    };
    for element in elements {
        let Some((lat, lon)) = element_position(element) else {
            continue;
        };
        let no_tags = serde_json::Value::Null;
        let tags = element.get("tags").unwrap_or(&no_tags);

        let kind = match tag_text(tags, "amenity").as_str() {
            "atm" => "atm",
            "bank" => "bank_with_atm",
            _ => "poi_with_atm",
        };

        let mut street_name = tag_text(tags, "addr:street");
        if street_name.is_empty() {
            street_name = tag_text(tags, "addr:place");
        }
        let house_number = tag_text(tags, "addr:housenumber");
        let street = join_non_empty(&[&street_name, &house_number], " ");

        let mut operator = tag_text(tags, "operator");
        if operator.is_empty() {
            operator = tag_text(tags, "brand");
        }
        let mut check_date = tag_text(tags, "check_date");
        if check_date.is_empty() {
            check_date = tag_text(tags, "survey:date");
        }

        let element_type = element
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("node");
        let element_id = element
            .get("id")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);

        records.push(Atm {
            lat,
            lon,
            name: tag_text(tags, "name"),
            operator,
            brand: tag_text(tags, "brand"),
            street,
            postcode: tag_text(tags, "addr:postcode"),
            city: tag_text(tags, "addr:city"),
            kind: kind.to_string(),
            opening_hours: tag_text(tags, "opening_hours"),
            fee: tag_text(tags, "fee"),
            cash_in: tag_text(tags, "cash_in"),
            wheelchair: tag_text(tags, "wheelchair"),
            indoor: tag_text(tags, "indoor"),
            check_date,
            source: "osm".to_string(),
            source_id: format!("{element_type}/{element_id}"),
            url: format!("https://www.openstreetmap.org/{element_type}/{element_id}"),
            address_source: String::new(),
            place: place.to_string(),
            merged: Vec::new(),
        });
    }
    records
}

/// Copy `value` into `target` when the target is still empty.
fn fill_if_empty(target: &mut String, value: &str) {
    if target.is_empty() && !value.is_empty() {
        *target = value.to_string();
    }
}

/// Let the cluster head take over every descriptive attribute it lacks from a merged record.
fn inherit_missing_attributes(head: &mut Atm, record: &Atm) {
    fill_if_empty(&mut head.name, &record.name);
    fill_if_empty(&mut head.operator, &record.operator);
    fill_if_empty(&mut head.brand, &record.brand);
    fill_if_empty(&mut head.street, &record.street);
    fill_if_empty(&mut head.postcode, &record.postcode);
    fill_if_empty(&mut head.city, &record.city);
    fill_if_empty(&mut head.opening_hours, &record.opening_hours);
    fill_if_empty(&mut head.fee, &record.fee);
    fill_if_empty(&mut head.cash_in, &record.cash_in);
    fill_if_empty(&mut head.wheelchair, &record.wheelchair);
    fill_if_empty(&mut head.indoor, &record.indoor);
    fill_if_empty(&mut head.check_date, &record.check_date);
}

/// Index of the ATM that `record` belongs to, or `None` when it starts a new one.
///
/// Candidates are the ATMs in the grid cells around the record. The closest one within
/// `radius_m` wins; among equally distant candidates the later one wins, which keeps results
/// identical to the original implementation.
fn find_cluster_for(record: &Atm, atms: &[Atm], index: &GridIndex, radius_m: f64) -> Option<usize> {
    let record_operator = operator_key(&record.operator);
    let mut best: Option<usize> = None;
    let mut best_distance = radius_m;
    for candidate_index in index.near(record.lat, record.lon) {
        let candidate = &atms[candidate_index];

        // Two separately mapped machines from the same source are two machines, however close.
        let both_separately_mapped =
            record.kind == "atm" && candidate.kind == "atm" && record.source == candidate.source;
        if both_separately_mapped {
            continue;
        }

        // Different known banks never merge; an unknown operator merges with anything.
        let candidate_operator = operator_key(&candidate.operator);
        let both_known = !record_operator.is_empty() && !candidate_operator.is_empty();
        if both_known && record_operator != candidate_operator {
            continue;
        }

        let distance = haversine_m(record.lat, record.lon, candidate.lat, candidate.lon);
        if distance <= best_distance {
            best = Some(candidate_index);
            best_distance = distance;
        }
    }
    best
}

/// Fold records that describe the same machine into one ATM.
///
/// Records within `radius_m` metres of each other merge when their operators match (see
/// [`operator_key`]) or one of them is unknown, except that two separately mapped ATMs from the
/// same source always stay two machines. The most precise record (see [`kind_priority`]) becomes
/// the head, inherits the attributes it lacks from the others and keeps them in `merged`.
pub fn merge(mut records: Vec<Atm>, radius_m: f64) -> Vec<Atm> {
    if records.is_empty() {
        return records;
    }
    // Most precise records first, so that they become the heads the others attach to.
    records.sort_by_key(|record| kind_priority(&record.kind));

    // Cells twice the radius wide: a neighbour within the radius is always in the same or an
    // adjacent cell, so looking at the 3 × 3 block around a record is enough.
    let mut index = GridIndex::new(radius_m.max(1.0) * 2.0, records[0].lat);
    let mut atms: Vec<Atm> = Vec::new();
    for record in records {
        match find_cluster_for(&record, &atms, &index, radius_m) {
            None => {
                index.add(record.lat, record.lon, atms.len());
                atms.push(record);
            }
            Some(head_index) => {
                let head = &mut atms[head_index];
                inherit_missing_attributes(head, &record);
                head.merged.push(record);
            }
        }
    }
    atms
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atm(id: &str, kind: &str, operator: &str, lat: f64, lon: f64) -> Atm {
        Atm {
            lat,
            lon,
            operator: operator.into(),
            kind: kind.into(),
            source: "osm".into(),
            source_id: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn operator_keys() {
        assert_eq!(operator_key("Die Steiermärkische"), "sparkasse");
        assert_eq!(operator_key("Raiffeisenbank Graz-Straßgang"), "raiffeisen");
        assert_eq!(operator_key("Volksbank Steiermark"), "volksbank");
        assert_eq!(operator_key("Some Bank Ltd."), "somebankltd");
        assert_eq!(operator_key(""), "");
    }

    #[test]
    fn two_atm_nodes_stay_separate_but_bank_merges_into_atm() {
        let a = atm("node/1", "atm", "Sparkasse", 47.0, 15.0);
        let b = atm("node/2", "atm", "Sparkasse", 47.0 + 20.0 / 111_320.0, 15.0);
        let mut bank = atm(
            "node/3",
            "bank_with_atm",
            "Steiermärkische Sparkasse",
            47.0,
            15.0 + 10.0 / 75_800.0,
        );
        bank.street = "Hauptplatz 1".into();
        let out = merge(vec![bank, a, b], 40.0);
        assert_eq!(out.len(), 2);
        let head = out.iter().find(|x| x.source_id == "node/1").unwrap();
        assert_eq!(head.merged.len(), 1);
        assert_eq!(head.street, "Hauptplatz 1");
    }

    #[test]
    fn different_operators_do_not_merge_but_unknown_does() {
        let a = atm("node/1", "atm", "Sparkasse", 47.0, 15.0);
        let b = atm("node/2", "bank_with_atm", "Raiffeisen", 47.0, 15.0);
        let c = atm("node/3", "poi_with_atm", "", 47.0, 15.0);
        let out = merge(vec![a, b, c], 40.0);
        assert_eq!(out.len(), 2);
        assert_eq!(out.iter().map(|c| c.merged.len()).sum::<usize>(), 1);
    }

    #[test]
    fn parse_elements_maps_tags() {
        let data = serde_json::json!({"elements": [
            {"type": "node", "id": 1, "lat": 47.0, "lon": 15.0, "tags": {"amenity": "atm", "operator": "Sparkasse",
             "addr:street": "Hauptplatz", "addr:housenumber": "1", "survey:date": "2026-01-01"}},
            {"type": "way", "id": 2, "center": {"lat": 47.1, "lon": 15.1}, "tags": {"amenity": "bank", "atm": "yes",
             "brand": "Raiffeisen", "addr:place": "Dorf", "addr:housenumber": "3"}},
            {"type": "node", "id": 3, "tags": {"amenity": "atm"}}
        ]});
        let atms = parse_overpass_atms(&data, "Graz");
        assert_eq!(atms.len(), 2);
        assert_eq!(atms[0].street, "Hauptplatz 1");
        assert_eq!(atms[0].check_date, "2026-01-01");
        assert_eq!(atms[0].stable_id(), "osm:node/1");
        assert_eq!(atms[1].kind, "bank_with_atm");
        assert_eq!(atms[1].operator, "Raiffeisen");
        assert_eq!(atms[1].street, "Dorf 3");
        assert_eq!(atms[1].url, "https://www.openstreetmap.org/way/2");
    }

    #[test]
    fn address_line_uses_the_known_parts() {
        let mut a = atm("node/1", "atm", "", 47.0, 15.0);
        assert_eq!(a.address_line(), "");
        a.street = "Hauptplatz 1".into();
        assert_eq!(a.address_line(), "Hauptplatz 1");
        a.city = "Graz".into();
        assert_eq!(a.address_line(), "Hauptplatz 1, Graz");
        a.postcode = "8010".into();
        assert_eq!(a.address_line(), "Hauptplatz 1, 8010 Graz");
    }
}
