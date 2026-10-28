//! Place lists for batch exports, and file-name slugs.
//!
//! `data/places.txt` lists the places the *Export data* workflow refreshes, one per line as
//! `Name` or `Name|level`. The same syntax works comma-separated on the command line.

/// `(name, level)` pairs from a place list. Blank lines and lines starting with `#` are
/// skipped; several places may share one line, separated by commas; the level after `|` is
/// optional and comes back as an empty string.
pub fn read_places(text: &str) -> Vec<(String, String)> {
    let mut places = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        for entry in line.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let (name, level) = entry.split_once('|').unwrap_or((entry, ""));
            places.push((name.trim().to_string(), level.trim().to_string()));
        }
    }
    places
}

/// A lower-case ASCII file name for a place: umlauts and accents transliterated, every other
/// non-alphanumeric character turned into a single `-`. `Sankt Pölten` becomes `sankt-poelten`.
pub fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for ch in name.to_lowercase().chars() {
        let replacement = match ch {
            'ä' => "ae",
            'ö' => "oe",
            'ü' => "ue",
            'ß' => "ss",
            'á' | 'à' | 'â' => "a",
            'é' | 'è' | 'ê' => "e",
            'í' | 'ì' | 'î' => "i",
            'ó' | 'ò' | 'ô' => "o",
            'ú' | 'ù' | 'û' => "u",
            'ç' => "c",
            c if c.is_ascii_alphanumeric() => {
                slug.push(c);
                last_was_dash = false;
                continue;
            }
            _ => "",
        };
        if replacement.is_empty() {
            // A separator: collapse runs of them and never start with one.
            if !last_was_dash && !slug.is_empty() {
                slug.push('-');
                last_was_dash = true;
            }
        } else {
            slug.push_str(replacement);
            last_was_dash = false;
        }
    }
    slug.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_and_slugs() {
        let text = "# Name or Name|level (state, district, municipality, borough).\nGraz\nLeoben|municipality\n\nGraz, Linz|state ,, Wien\n";
        assert_eq!(
            read_places(text),
            vec![
                ("Graz".into(), "".into()),
                ("Leoben".into(), "municipality".into()),
                ("Graz".into(), "".into()),
                ("Linz".into(), "state".into()),
                ("Wien".into(), "".into())
            ]
        );
        assert_eq!(slugify("Bezirk Graz-Umgebung"), "bezirk-graz-umgebung");
        assert_eq!(slugify("Sankt Pölten"), "sankt-poelten");
        assert_eq!(slugify("Weiz "), "weiz");
        assert_eq!(slugify("Straß in Steiermark"), "strass-in-steiermark");
    }
}
