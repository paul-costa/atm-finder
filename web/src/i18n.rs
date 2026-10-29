//! The user-facing strings of the website in German and English.
//!
//! Every string has a key; the HTML marks elements with `data-i18n="key"` and the Rust side
//! looks keys up with [`Lang::t`]. Strings with `{placeholders}` are completed with [`fill`].
//! Keys that equal an admin-level key (`state`, `district`, ...) translate those levels.

/// The two languages of the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    De,
    En,
}

impl Lang {
    /// BCP 47 code for the `lang` attribute: `de` or `en`.
    pub fn code(self) -> &'static str {
        match self {
            Lang::De => "de",
            Lang::En => "en",
        }
    }

    /// The language for a browser language code: anything starting with `de` is German,
    /// everything else English.
    pub fn from_code(code: &str) -> Lang {
        if code.to_lowercase().starts_with("de") {
            Lang::De
        } else {
            Lang::En
        }
    }

    /// The translation of `key`, or `?` for an unknown key so that a typo is visible on the page.
    pub fn t(self, key: &str) -> &'static str {
        for (candidate, german, english) in STRINGS {
            if *candidate == key {
                return match self {
                    Lang::De => german,
                    Lang::En => english,
                };
            }
        }
        "?"
    }
}

/// `(key, German, English)` for every string on the page.
const STRINGS: &[(&str, &str, &str)] = &[
    ("title", "Bankomaten in Österreich", "ATMs in Austria"),
    ("tagline", "Alle Bankomaten einer Gemeinde, eines Bezirks oder Bundeslands aus OpenStreetMap, als Karte und Download.",
     "Every ATM of a municipality, district or state from OpenStreetMap, as a map and as a download."),
    ("search_label", "Ort", "Place"),
    ("search_placeholder", "z. B. Graz, Leoben, Graz-Umgebung, Steiermark", "e.g. Graz, Leoben, Graz-Umgebung, Steiermark"),
    ("search_button", "Suchen", "Search"),
    // Admin levels, keyed like `boundary::level_key`.
    ("country", "Österreich", "Austria"),
    ("state", "Bundesland", "state"),
    ("district", "Bezirk", "district"),
    ("municipality", "Gemeinde", "municipality"),
    ("borough", "Stadtbezirk", "borough"),
    ("other", "Gebiet", "area"),
    // A statutory city (level 6 with a municipality code), keyed like `Boundary::kind_key`.
    ("city", "Stadt", "city"),
    ("candidates_title", "Welches Gebiet?", "Which area?"),
    ("no_match", "Kein Gebiet mit diesem Namen gefunden. Tipp: Ein Teil des Namens genügt, z. B. „Klagenfurt“.",
     "No area with this name. Tip: part of the name is enough, e.g. “Klagenfurt”."),
    ("status_searching", "Suche Gebiet …", "Looking up the area …"),
    ("status_boundary", "Lade Gebietsgrenze …", "Loading the boundary …"),
    ("status_atms", "Lade Bankomaten aus OpenStreetMap … (bei Bundesländern bis zu einer Minute)",
     "Loading ATMs from OpenStreetMap … (up to a minute for a state)"),
    ("status_retry", "OpenStreetMap-Server ausgelastet, neuer Versuch …", "OpenStreetMap server busy, retrying …"),
    ("status_mirror", "Wechsle auf Ersatzserver …", "Switching to a mirror server …"),
    ("status_addresses", "Ergänze Adressen aus OpenStreetMap …", "Completing addresses from OpenStreetMap …"),
    ("status_reverse", "Rückwärtssuche über Nominatim (1 Anfrage pro Sekunde) …", "Reverse geocoding via Nominatim (1 request per second) …"),
    ("result_summary", "{n} Bankomaten in {place} ({raw} Rohtreffer, Duplikate zusammengeführt).",
     "{n} ATMs in {place} ({raw} raw records, duplicates merged)."),
    ("result_none", "In diesem Gebiet sind in OpenStreetMap keine Bankomaten erfasst.", "OpenStreetMap has no ATMs in this area."),
    ("addresses_missing", "{n} Einträge ohne Adresse.", "{n} records without an address."),
    ("fill_button", "Adressen ergänzen", "Complete addresses"),
    ("fill_done", "Adressen ergänzt: {nearby} über nächste Hausnummer, {reverse} per Rückwärtssuche, {open} offen.",
     "Addresses completed: {nearby} from the nearest house number, {reverse} reverse geocoded, {open} unresolved."),
    ("copy_link", "Link kopieren", "Copy link"),
    ("link_copied", "Link kopiert ✓", "Link copied ✓"),
    ("download_title", "Download", "Download"),
    ("download_hint", "CSV für Tabellen, GeoJSON für Karten-Software, KML für Google My Maps.",
     "CSV for spreadsheets, GeoJSON for mapping software, KML for Google My Maps."),
    ("list_title", "Liste", "List"),
    ("open_osm", "OpenStreetMap", "OpenStreetMap"),
    ("open_gmaps", "Google Maps", "Google Maps"),
    ("no_address", "ohne Adresse", "no address"),
    ("ready_title", "Fertige Dateien", "Ready-made files"),
    ("ready_intro", "Regelmäßig erzeugte Exporte, ohne Wartezeit:", "Exports generated regularly, no waiting:"),
    ("ready_rows", "{n} Bankomaten", "{n} ATMs"),
    ("ready_show", "Auf der Karte zeigen", "Show on the map"),
    ("gmaps_mobile", "Am Handy öffnen sich die Google-Maps-Listen nur in der Google-Maps-App; im Browser zeigt Google „Liste nicht gefunden“.", "On a phone the Google Maps lists open only in the Google Maps app; in the browser Google says “List not found”."),
    ("gmaps_create", "Google-Maps-Liste erstellen", "Create Google Maps list"),
    ("gmaps_open", "Google-Maps-Liste öffnen", "Open Google Maps list"),
    ("gmaps_partial", "{n} von {total} Bankomaten sind in der Liste; die übrigen führt Google nicht als eigenen Ort (alle stehen in CSV und KML).", "{n} of {total} ATMs are in the list; Google does not know the others as places (CSV and KML have all of them)."),
    ("gmaps_running_this", "Die Liste wird gerade erstellt, das dauert einige Minuten. Diese Seite aktualisiert sich von selbst.", "The list is being created, this takes a few minutes. This page updates by itself."),
    ("gmaps_running_other", "Gerade wird die Liste für {place} erstellt. Danach ist das Erstellen 30 Minuten lang gesperrt.", "The list for {place} is being created right now. After that, creating is locked for 30 minutes."),
    ("gmaps_cooldown", "Neue Listen sind ab {time} wieder möglich.", "New lists are possible again from {time}."),
    ("gmaps_failed", "Liste konnte nicht erstellt werden: {msg}", "Could not create the list: {msg}"),
    ("gmaps_quota_hour", "Von deinem Anschluss aus ist eine Liste pro Stunde möglich; die nächste ab {time}.", "One list per hour is possible from your connection; the next one from {time}."),
    ("gmaps_quota_day", "Von deinem Anschluss aus sind {n} Listen pro Tag möglich; die nächste ab {time}.", "{n} lists per day are possible from your connection; the next one from {time}."),
    ("gmaps_quota_blocked", "Von deinem Anschluss aus wurden schon {n} Listen erstellt; weitere sind nicht möglich. Die fertigen Listen und CSV/KML bleiben verfügbar.", "{n} lists have already been created from your connection; no more are possible. The existing lists and CSV/KML stay available."),
    ("error", "Fehler: {msg}", "Error: {msg}"),
    ("error_overpass", "OpenStreetMap-Server nicht erreichbar oder ausgelastet. Bitte später noch einmal versuchen.",
     "The OpenStreetMap server is unreachable or overloaded. Please try again later."),
    ("footer", "Daten © OpenStreetMap-Mitwirkende (ODbL). Quellcode und Kommandozeilenwerkzeug auf GitHub.",
     "Data © OpenStreetMap contributors (ODbL). Source code and command-line tool on GitHub."),
    // The toggle shows the *other* language.
    ("lang_switch", "English", "Deutsch"),
    ("map_popup_hours", "Öffnungszeiten", "Opening hours"),
    ("map_boundary", "Gebietsgrenze", "Boundary"),
];

/// Replace `{name}` placeholders in a translated string with the given values.
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut text = template.to_string();
    for (name, value) in values {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_and_fill() {
        assert_eq!(Lang::De.t("search_button"), "Suchen");
        assert_eq!(Lang::En.t("search_button"), "Search");
        assert_eq!(Lang::En.t("nope"), "?");
        assert_eq!(Lang::from_code("de-AT"), Lang::De);
        assert_eq!(Lang::from_code("en-US"), Lang::En);
        assert_eq!(
            fill(
                Lang::En.t("result_summary"),
                &[("n", "3"), ("place", "Graz"), ("raw", "4")]
            ),
            "3 ATMs in Graz (4 raw records, duplicates merged)."
        );
    }

    #[test]
    fn every_key_has_both_languages() {
        for (k, de, en) in STRINGS {
            assert!(!de.is_empty() && !en.is_empty(), "{k}");
        }
    }
}
