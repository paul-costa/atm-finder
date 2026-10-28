//! Austrian administrative boundaries: Overpass queries, parsing, and choosing among name matches.
//!
//! A *boundary* is an OpenStreetMap relation tagged `boundary=administrative`. Its `admin_level`
//! says what kind of area it is; in Austria the levels are 2 country, 4 state (Bundesland),
//! 6 district (politischer Bezirk, and also the statutory cities such as Graz, which are both a
//! district and a municipality), 8 municipality (Gemeinde) and 9 borough (the Gemeindebezirke
//! of Vienna and the Stadtbezirke of Graz).
//!
//! Municipalities carry the five-digit municipality code `ref:at:gkz` (Gemeindekennziffer),
//! districts a three-digit one; the first digit of either is the state, which lets us label a
//! match with its state without another request. States carry `ISO3166-2` (`AT-6`) instead.
//!
//! Overpass turns a relation into a searchable *area* by adding 3 600 000 000 to its id, so
//! `area(3600016239)` is Austria. `out tags` returns only tags, `out body` adds the members of a
//! relation with their roles, and `out geom` adds the positions of a way.

use std::collections::HashMap;

use crate::geo::{build_multipolygon, Coord, MultiPolygon};

/// OSM relation id of Austria.
pub const AUSTRIA_RELATION: u64 = 16239;
/// Admin levels a place name is looked up at: state, district, municipality, borough.
pub const LOOKUP_LEVELS: [u8; 4] = [4, 6, 8, 9];
/// When several boundaries share a name, this is the order of preference: municipality first
/// (a search for "Salzburg" usually means the city), then district, state, borough, country.
pub const PREFERRED_ORDER: [u8; 5] = [8, 6, 4, 9, 2];

/// An administrative boundary. `geometry` is `None` until it has been loaded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Boundary {
    pub relation_id: u64,
    pub name: String,
    pub admin_level: u8,
    /// All tags of the relation.
    pub tags: HashMap<String, String>,
    pub geometry: Option<MultiPolygon>,
}

/// Human-readable key of an admin level: `country`, `state`, `district`, `municipality`,
/// `borough`, or `other` for anything else.
pub fn level_key(level: u8) -> &'static str {
    match level {
        2 => "country",
        4 => "state",
        6 => "district",
        8 => "municipality",
        9 => "borough",
        _ => "other",
    }
}

/// The admin level for a key produced by [`level_key`]; `None` for unknown keys.
pub fn level_for(key: &str) -> Option<u8> {
    match key {
        "country" => Some(2),
        "state" => Some(4),
        "district" => Some(6),
        "municipality" => Some(8),
        "borough" => Some(9),
        _ => None,
    }
}

/// The state that a municipality code or an `AT-n` ISO code starts with.
fn state_name(code: &str) -> &'static str {
    match code.chars().next() {
        Some('1') => "Burgenland",
        Some('2') => "Kärnten",
        Some('3') => "Niederösterreich",
        Some('4') => "Oberösterreich",
        Some('5') => "Salzburg",
        Some('6') => "Steiermark",
        Some('7') => "Tirol",
        Some('8') => "Vorarlberg",
        Some('9') => "Wien",
        _ => "",
    }
}

impl Boundary {
    /// See [`level_key`].
    pub fn level_key(&self) -> &'static str {
        level_key(self.admin_level)
    }

    /// What to call the boundary on screen: [`Boundary::level_key`], except that a statutory
    /// city (level 6 with a five-digit municipality code, such as Graz) is a `city`.
    pub fn kind_key(&self) -> &'static str {
        if self.admin_level == 6 && self.gkz().len() == 5 {
            "city"
        } else {
            self.level_key()
        }
    }

    /// The municipality or district code (`ref:at:gkz`), empty when the relation has none.
    pub fn gkz(&self) -> &str {
        match self.tags.get("ref:at:gkz") {
            Some(code) => code,
            None => "",
        }
    }

    /// Name of the state the boundary lies in: the boundary's own name for a state, empty for
    /// the country, otherwise derived from the GKZ or the ISO code; empty when neither exists.
    pub fn state(&self) -> String {
        if self.admin_level == 4 {
            return self.name.clone();
        }
        if self.admin_level == 2 {
            return String::new();
        }
        let gkz = self.gkz();
        if !gkz.is_empty() {
            return state_name(gkz).to_string();
        }
        let iso_code = match self.tags.get("ISO3166-2") {
            Some(code) => code.replace("AT-", ""),
            None => String::new(),
        };
        state_name(&iso_code).to_string()
    }

    /// Whether this is a Gemeinde: level 8, or a statutory city at level 6, recognisable by its
    /// five-digit municipality code (districts have three digits).
    pub fn is_municipality(&self) -> bool {
        self.admin_level == 8 || self.gkz().len() == 5
    }

    /// One line for logs and error messages, in English:
    /// `Graz (city, Steiermark, GKZ 60101, relation 34719)`.
    pub fn describe(&self) -> String {
        let mut details = vec![self.kind_key().to_string()];
        let state = self.state();
        let is_state_or_country = self.admin_level == 2 || self.admin_level == 4;
        if !state.is_empty() && !is_state_or_country {
            details.push(state);
        }
        if !self.gkz().is_empty() {
            details.push(format!("GKZ {}", self.gkz()));
        }
        format!(
            "{} ({}, relation {})",
            self.name,
            details.join(", "),
            self.relation_id
        )
    }

    /// Link to the relation on openstreetmap.org.
    pub fn url(&self) -> String {
        format!(
            "https://www.openstreetmap.org/relation/{}",
            self.relation_id
        )
    }

    /// Whether a position lies inside the boundary. Without loaded geometry everything counts
    /// as inside, so that callers which only have the id still work.
    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        match &self.geometry {
            Some(geometry) => geometry.contains(lat, lon),
            None => true,
        }
    }

    /// The boundary as a GeoJSON feature with name, level, state and relation id as properties.
    pub fn to_feature(&self) -> serde_json::Value {
        let geometry = match &self.geometry {
            Some(geometry) => geometry.to_geojson(),
            None => serde_json::Value::Null,
        };
        serde_json::json!({
            "type": "Feature",
            "properties": {
                "name": self.name,
                "admin_level": self.admin_level,
                "level": self.level_key(),
                "state": self.state(),
                "relation": self.relation_id,
                "url": self.url(),
            },
            "geometry": geometry,
        })
    }
}

/// Escape a name so that it can be used literally inside an Overpass regular expression that
/// itself sits inside a double-quoted Overpass QL string.
pub fn regex_literal(name: &str) -> String {
    let mut escaped = String::new();
    for ch in name.chars() {
        if ".^$*+?()[]{}|\\".contains(ch) {
            escaped.push('\\');
            escaped.push(ch);
        } else if ch == '"' {
            escaped.push_str("\\\"");
        } else {
            escaped.push(ch);
        }
    }
    escaped
}

/// Overpass regular expression for "Sankt" or "St.", with or without a space after it.
const SAINT_PATTERN: &str = "(Sankt|St\\.) ?";

/// Overpass regular expression for a typed place name: the words literally, except that
/// `Sankt`, `St.` and `St` stand for each other, because OSM spells both (`St. Pölten`,
/// `Sankt Veit in Defereggen`) and people type either.
pub fn name_regex(name: &str) -> String {
    // (text, is a saint word) per word; "St.Pölten" without a space is two words.
    let mut words: Vec<(String, bool)> = Vec::new();
    for word in name.split_whitespace() {
        let lower = word.to_lowercase();
        if lower == "sankt" || lower == "st." || lower == "st" {
            words.push((SAINT_PATTERN.to_string(), true));
        } else if lower.starts_with("st.") {
            words.push((SAINT_PATTERN.to_string(), true));
            words.push((regex_literal(&word[3..]), false));
        } else {
            words.push((regex_literal(word), false));
        }
    }
    let mut pattern = String::new();
    for (index, (text, is_saint)) in words.iter().enumerate() {
        pattern.push_str(text);
        // The saint pattern brings its own optional space.
        if !is_saint && index + 1 < words.len() {
            pattern.push(' ');
        }
    }
    pattern
}

/// Overpass query for the administrative boundaries in Austria whose `name` equals `name`
/// (`exact`) or contains it, case-insensitively, at the given admin levels (see
/// [`name_regex`] for `Sankt`/`St.`). Returns tags only.
pub fn candidates_query(name: &str, levels: &[u8], exact: bool) -> String {
    let level_alternatives: Vec<String> = levels.iter().map(|level| level.to_string()).collect();
    let level_pattern = level_alternatives.join("|");
    let name_pattern = if exact {
        format!("^{}$", name_regex(name))
    } else {
        name_regex(name)
    };
    let austria_area = 3_600_000_000u64 + AUSTRIA_RELATION;
    format!(
        "[out:json][timeout:60];\n\
         area({austria_area})->.at;\n\
         rel[\"boundary\"=\"administrative\"][\"admin_level\"~\"^({level_pattern})$\"][\"name\"~\"{name_pattern}\",i](area.at);\n\
         out tags;"
    )
}

/// Overpass query for the relation itself (tags and members) and the positions of its member ways.
pub fn geometry_query(relation_id: u64) -> String {
    format!(
        "[out:json][timeout:180];\n\
         rel({relation_id})->.r;\n\
         .r out body;\n\
         way(r.r);\n\
         out geom;"
    )
}

/// Server-side time limit in seconds for the ATM query: the whole country and a state need more.
pub fn atms_timeout(admin_level: u8) -> u32 {
    match admin_level {
        2 => 900,
        4 => 600,
        _ => 300,
    }
}

/// Overpass query for every ATM inside the boundary: separately mapped machines, bank branches
/// with a machine, and other places with a machine (see the `model` module). `out center`
/// gives ways and relations a centre position.
pub fn atms_query(relation_id: u64, admin_level: u8) -> String {
    let timeout = atms_timeout(admin_level);
    format!(
        "[out:json][timeout:{timeout}];\n\
         rel({relation_id});\n\
         map_to_area->.a;\n\
         (\n  nwr[\"amenity\"=\"atm\"](area.a);\n  \
         nwr[\"amenity\"=\"bank\"][\"atm\"=\"yes\"](area.a);\n  \
         nwr[\"atm\"=\"yes\"][\"amenity\"!~\"^(atm|bank)$\"](area.a);\n);\n\
         out center tags;"
    )
}

fn element_type(element: &serde_json::Value) -> &str {
    element
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("")
}

fn element_id(element: &serde_json::Value) -> u64 {
    element
        .get("id")
        .and_then(|value| value.as_u64())
        .unwrap_or(0)
}

/// The string-valued tags of an element.
fn element_tags(element: &serde_json::Value) -> HashMap<String, String> {
    let mut tags = HashMap::new();
    let Some(object) = element.get("tags").and_then(|tags| tags.as_object()) else {
        return tags;
    };
    for (key, value) in object {
        if let Some(text) = value.as_str() {
            tags.insert(key.clone(), text.to_string());
        }
    }
    tags
}

/// Boundaries from the response to [`candidates_query`], sorted by level, name and id.
/// Elements that are not relations or have no numeric `admin_level` are ignored.
pub fn parse_candidates(data: &serde_json::Value) -> Vec<Boundary> {
    let mut boundaries = Vec::new();
    let elements = data
        .get("elements")
        .and_then(|elements| elements.as_array());
    for element in elements.into_iter().flatten() {
        if element_type(element) != "relation" {
            continue;
        }
        let tags = element_tags(element);
        let Some(admin_level) = tags
            .get("admin_level")
            .and_then(|level| level.parse::<u8>().ok())
        else {
            continue;
        };
        let id = element_id(element);
        let name = match tags.get("name") {
            Some(name) => name.clone(),
            None => format!("relation {id}"),
        };
        boundaries.push(Boundary {
            relation_id: id,
            name,
            admin_level,
            tags,
            geometry: None,
        });
    }
    boundaries.sort_by(|a, b| {
        (a.admin_level, &a.name, a.relation_id).cmp(&(b.admin_level, &b.name, b.relation_id))
    });
    boundaries
}

/// Roles of the member ways of a relation element, by way id. Ways without a role count as
/// `outer` later on, which is how OSM treats them.
fn member_way_roles(relation: &serde_json::Value) -> HashMap<u64, String> {
    let mut roles = HashMap::new();
    let members = relation
        .get("members")
        .and_then(|members| members.as_array());
    for member in members.into_iter().flatten() {
        if element_type(member) != "way" {
            continue;
        }
        let Some(way_id) = member.get("ref").and_then(|value| value.as_u64()) else {
            continue;
        };
        let role = member
            .get("role")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        roles.insert(way_id, role.to_string());
    }
    roles
}

/// The positions of a way from `out geom`, in way order.
fn way_positions(way: &serde_json::Value) -> Option<Vec<Coord>> {
    let geometry = way.get("geometry")?.as_array()?;
    let mut positions = Vec::new();
    for point in geometry {
        let lon = point.get("lon")?.as_f64()?;
        let lat = point.get("lat")?.as_f64()?;
        positions.push((lon, lat));
    }
    Some(positions)
}

/// Tags and polygon of a relation from the response to [`geometry_query`].
///
/// Fails when the relation is not in the response (it does not exist) or has no outer ways
/// (it is not a boundary).
pub fn parse_geometry(
    data: &serde_json::Value,
    relation_id: u64,
) -> Result<(HashMap<String, String>, MultiPolygon), String> {
    let elements = match data
        .get("elements")
        .and_then(|elements| elements.as_array())
    {
        Some(elements) => elements.clone(),
        None => Vec::new(),
    };
    let relation = elements
        .iter()
        .find(|element| element_type(element) == "relation" && element_id(element) == relation_id)
        .ok_or_else(|| format!("relation {relation_id} does not exist in OpenStreetMap"))?;
    let roles = member_way_roles(relation);

    let mut outer_ways = Vec::new();
    let mut inner_ways = Vec::new();
    for element in &elements {
        if element_type(element) != "way" {
            continue;
        }
        let Some(positions) = way_positions(element) else {
            continue;
        };
        let role = roles.get(&element_id(element)).map(String::as_str);
        if role == Some("inner") {
            inner_ways.push(positions);
        } else {
            outer_ways.push(positions);
        }
    }

    let geometry = build_multipolygon(outer_ways, inner_ways);
    if geometry.polygons.is_empty() {
        return Err(format!(
            "relation {relation_id} has no outer ways; is it a boundary?"
        ));
    }
    Ok((element_tags(relation), geometry))
}

/// Prefixes that OSM names carry but people leave out when they type a place.
const NAME_PREFIXES: &[&str] = &[
    "politischer bezirk ",
    "bezirk ",
    "stadtgemeinde ",
    "marktgemeinde ",
    "gemeinde ",
    "stadt ",
];

/// A place name as spelled, for comparison: lower case, single spaces, `St.` expanded to
/// `Sankt`. `St.Pölten` and `sankt  pölten` compare equal.
fn spelled(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    let mut words: Vec<String> = Vec::new();
    for word in lower.split_whitespace() {
        if word == "st." {
            words.push("sankt".to_string());
        } else if let Some(rest) = word.strip_prefix("st.") {
            // "St.Pölten" written without a space.
            words.push("sankt".to_string());
            words.push(rest.to_string());
        } else {
            words.push(word.to_string());
        }
    }
    words.join(" ")
}

/// Comparison form of a place name: [`spelled`], and a leading prefix such as `Bezirk `
/// removed. `Bezirk Graz-Umgebung` and `graz-umgebung` compare equal.
pub fn normalize_name(name: &str) -> String {
    let mut normalized = spelled(name);
    for prefix in NAME_PREFIXES {
        if let Some(rest) = normalized.strip_prefix(prefix) {
            normalized = rest.to_string();
            break;
        }
    }
    normalized.trim().to_string()
}

/// Choose one boundary among candidates that share a name, without asking the user.
///
/// Levels are tried in [`PREFERRED_ORDER`] (then any other levels, ascending): the first level
/// with exactly one candidate wins. A level with several candidates, for example two villages
/// called Neukirchen, is ambiguous; they are returned as the error so the caller can list them.
/// No candidates at all gives an empty error list.
pub fn pick(candidates: &[Boundary]) -> Result<Boundary, Vec<Boundary>> {
    let mut levels: Vec<u8> = PREFERRED_ORDER.to_vec();
    let mut other_levels: Vec<u8> = candidates
        .iter()
        .map(|boundary| boundary.admin_level)
        .filter(|level| !levels.contains(level))
        .collect();
    other_levels.sort();
    other_levels.dedup();
    levels.extend(other_levels);

    for level in levels {
        let at_level: Vec<&Boundary> = candidates
            .iter()
            .filter(|boundary| boundary.admin_level == level)
            .collect();
        match at_level.len() {
            0 => continue,
            1 => return Ok(at_level[0].clone()),
            _ => return Err(at_level.into_iter().cloned().collect()),
        }
    }
    Err(Vec::new())
}

/// Position of a level in [`PREFERRED_ORDER`]; unknown levels sort last.
fn preference_rank(level: u8) -> usize {
    PREFERRED_ORDER
        .iter()
        .position(|&preferred| preferred == level)
        .unwrap_or(PREFERRED_ORDER.len())
}

/// Sort candidates for display: names equal to the query first, then by level preference,
/// then by name and id.
pub fn rank(candidates: &mut [Boundary], query: &str) {
    let wanted = normalize_name(query);
    candidates.sort_by_key(|boundary| {
        let name_differs = normalize_name(&boundary.name) != wanted;
        (
            name_differs,
            preference_rank(boundary.admin_level),
            boundary.name.clone(),
            boundary.relation_id,
        )
    });
}

/// Index of the one candidate to use without asking, or `None` when the user has to choose.
///
/// Only candidates whose name equals the query (see [`normalize_name`]) qualify. Among them the
/// most preferred level wins; if that level has more than one match, the one spelled exactly
/// like the query (prefix included) wins, otherwise the choice is ambiguous.
pub fn auto_pick(candidates: &[Boundary], query: &str) -> Option<usize> {
    let wanted = normalize_name(query);
    let matching: Vec<usize> = (0..candidates.len())
        .filter(|&index| normalize_name(&candidates[index].name) == wanted)
        .collect();
    let best_rank = matching
        .iter()
        .map(|&index| preference_rank(candidates[index].admin_level))
        .min()?;
    let at_best_rank: Vec<usize> = matching
        .into_iter()
        .filter(|&index| preference_rank(candidates[index].admin_level) == best_rank)
        .collect();
    if at_best_rank.len() == 1 {
        return Some(at_best_rank[0]);
    }
    // A statutory city and the district around it can share a name ("St. Pölten" and
    // "Bezirk St. Pölten"); then the one whose full name was typed wins.
    let typed = spelled(query);
    let spelled_alike: Vec<usize> = at_best_rank
        .into_iter()
        .filter(|&index| spelled(&candidates[index].name) == typed)
        .collect();
    if spelled_alike.len() == 1 {
        Some(spelled_alike[0])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(id: u64, name: &str, level: u8, gkz: &str) -> Boundary {
        let mut tags = HashMap::new();
        if !gkz.is_empty() {
            tags.insert("ref:at:gkz".to_string(), gkz.to_string());
        }
        Boundary {
            relation_id: id,
            name: name.into(),
            admin_level: level,
            tags,
            geometry: None,
        }
    }

    #[test]
    fn query_escapes_and_modes() {
        let q = candidates_query("Veit (Glan) \"x\"", &[6, 8], true);
        assert!(q.contains("^Veit \\(Glan\\) \\\"x\\\"$"));
        assert_eq!(name_regex("St. Pölten"), "(Sankt|St\\.) ?Pölten");
        assert_eq!(
            name_regex("Sankt  Veit an der Glan"),
            "(Sankt|St\\.) ?Veit an der Glan"
        );
        assert_eq!(name_regex("st.Pölten"), "(Sankt|St\\.) ?Pölten");
        assert_eq!(name_regex("Bad St Leonhard"), "Bad (Sankt|St\\.) ?Leonhard");
        assert_eq!(name_regex("Stainz"), "Stainz");
        assert!(q.contains("^(6|8)$") && q.contains("area(3600016239)"));
        assert!(!candidates_query("Umgebung", &LOOKUP_LEVELS, false).contains("^Umgebung"));
        assert!(
            atms_query(34719, 6).contains("[timeout:300]")
                && atms_query(16239, 2).contains("[timeout:900]")
        );
    }

    #[test]
    fn candidates_and_state() {
        let data = serde_json::json!({"elements": [
            {"type": "relation", "id": 34719, "tags": {"name": "Graz", "admin_level": "6", "ref:at:gkz": "60101"}},
            {"type": "relation", "id": 1, "tags": {"name": "Salzburg", "admin_level": "4", "ISO3166-2": "AT-5"}},
            {"type": "node", "id": 2, "tags": {"name": "x", "admin_level": "8"}},
            {"type": "relation", "id": 3, "tags": {"name": "y"}}
        ]});
        let c = parse_candidates(&data);
        assert_eq!(c.len(), 2);
        assert_eq!(
            (c[0].name.as_str(), c[0].state().as_str()),
            ("Salzburg", "Salzburg")
        );
        assert_eq!(
            (
                c[1].name.as_str(),
                c[1].state().as_str(),
                c[1].is_municipality()
            ),
            ("Graz", "Steiermark", true)
        );
        assert!(!b(5, "Bezirk Graz-Umgebung", 6, "606").is_municipality());
    }

    #[test]
    fn geometry_with_hole() {
        let data = serde_json::json!({"elements": [
            {"type": "relation", "id": 7, "tags": {"name": "X", "admin_level": "8"},
             "members": [{"type": "way", "ref": 1, "role": "outer"}, {"type": "way", "ref": 2, "role": "inner"}]},
            {"type": "way", "id": 1, "geometry": [{"lon": 0.0, "lat": 0.0}, {"lon": 10.0, "lat": 0.0}, {"lon": 10.0, "lat": 10.0}, {"lon": 0.0, "lat": 10.0}, {"lon": 0.0, "lat": 0.0}]},
            {"type": "way", "id": 2, "geometry": [{"lon": 4.0, "lat": 4.0}, {"lon": 6.0, "lat": 4.0}, {"lon": 6.0, "lat": 6.0}, {"lon": 4.0, "lat": 6.0}, {"lon": 4.0, "lat": 4.0}]}
        ]});
        let (tags, geom) = parse_geometry(&data, 7).unwrap();
        assert_eq!(tags.get("name").unwrap(), "X");
        assert!(geom.contains(1.0, 1.0) && !geom.contains(5.0, 5.0));
        assert!(parse_geometry(&data, 8).is_err());
    }

    #[test]
    fn normalisation_rank_and_auto_pick() {
        assert_eq!(normalize_name("Bezirk Graz-Umgebung"), "graz-umgebung");
        assert_eq!(normalize_name("St. Pölten"), "sankt pölten");
        assert_eq!(normalize_name("St.Pölten"), "sankt pölten");
        let mut c = vec![
            b(1, "Salzburg", 4, ""),
            b(2, "Salzburg-Umgebung", 6, "503"),
            b(3, "Salzburg", 8, "50101"),
        ];
        rank(&mut c, "salzburg");
        assert_eq!(
            c.iter().map(|x| x.relation_id).collect::<Vec<_>>(),
            vec![3, 1, 2]
        );
        assert_eq!(auto_pick(&c, "Salzburg"), Some(0));
        let two = vec![
            b(30, "Neukirchen", 8, "40000"),
            b(31, "Neukirchen", 8, "50000"),
        ];
        assert_eq!(auto_pick(&two, "neukirchen"), None);
        assert_eq!(
            auto_pick(&[b(40, "Bezirk Graz-Umgebung", 6, "606")], "Graz-Umgebung"),
            Some(0)
        );
        assert_eq!(
            auto_pick(&[b(41, "Sankt Veit an der Glan", 8, "")], "Veit"),
            None
        );
        let city_and_district = vec![
            b(63619, "Bezirk St. Pölten", 6, "319"),
            b(134351, "St. Pölten", 6, "30201"),
        ];
        assert_eq!(auto_pick(&city_and_district, "Sankt Pölten"), Some(1));
        assert_eq!(auto_pick(&city_and_district, "Bezirk St. Pölten"), Some(0));
        assert_eq!(pick(&c).unwrap().relation_id, 3);
        assert_eq!(pick(&two).unwrap_err().len(), 2);
        assert_eq!(
            b(34719, "Graz", 6, "60101").describe(),
            "Graz (city, Steiermark, GKZ 60101, relation 34719)"
        );
        assert_eq!(b(34719, "Graz", 6, "60101").kind_key(), "city");
        assert_eq!(
            b(5, "Bezirk Graz-Umgebung", 6, "606").kind_key(),
            "district"
        );
        assert_eq!(b(109166, "Wien", 4, "90001").kind_key(), "state");
    }
}
