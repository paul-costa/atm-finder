//! Configuration values such as API keys.
//!
//! A value is taken from the process environment first and, when it is not set there, from a
//! `.env` file in the current directory (`GOOGLE_MAPS_API_KEY=AIza...`). The file is read once
//! per run. This keeps keys out of the command line and out of shell history.

use std::collections::HashMap;
use std::sync::OnceLock;

/// The parsed `.env` file, read on first use.
static DOTENV: OnceLock<HashMap<String, String>> = OnceLock::new();

/// Parse the text of a `.env` file into key/value pairs.
///
/// Accepted lines are `KEY=value`, `export KEY=value`, values in single or double quotes, blank
/// lines and `#` comments. Anything else is ignored rather than reported, because a stray line
/// in a config file should not stop an export.
pub fn parse_dotenv(text: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((raw_key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let key = raw_key.trim();
        let key = key.strip_prefix("export ").unwrap_or(key).trim();
        values.insert(key.to_string(), unquote(raw_value.trim()).to_string());
    }
    values
}

/// Remove one pair of matching quotes around a value, if present.
fn unquote(value: &str) -> &str {
    let quoted = value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')));
    if quoted {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// The value of `name` from the environment, else from `.env`, else `None`.
pub fn get_env(name: &str) -> Option<String> {
    if let Ok(value) = std::env::var(name) {
        return Some(value);
    }
    let dotenv = DOTENV.get_or_init(|| {
        std::fs::read_to_string(".env")
            .map(|text| parse_dotenv(&text))
            .unwrap_or_default()
    });
    dotenv.get(name).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotenv_parsing() {
        let m = parse_dotenv("# c\nexport A=1\nB = \"two words\"\nC='x'\nbroken\n");
        assert_eq!(m.get("A").unwrap(), "1");
        assert_eq!(m.get("B").unwrap(), "two words");
        assert_eq!(m.get("C").unwrap(), "x");
        assert_eq!(m.len(), 3);
    }
}
