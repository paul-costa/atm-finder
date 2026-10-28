//! HTTP for the command-line tool: one small blocking client with the behaviour the public
//! OpenStreetMap servers expect from a polite tool.
//!
//! * Every request identifies the tool with a User-Agent that links to this repository.
//! * Overload (HTTP 429, 5xx) and network errors are retried with exponential backoff
//!   (5, 10, 20 seconds). The public Overpass instance regularly answers
//!   "502 at capacity" for a few seconds; waiting usually fixes it.
//! * When the main Overpass instance keeps failing, the query is sent to a public mirror.
//! * API keys and the Nominatim contact e-mail are blanked in every message, because logs end up
//!   in places others can read.

use std::fmt;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::env::get_env;
use crate::log::log;

/// The main public Overpass instance.
pub const DEFAULT_OVERPASS_URL: &str = "https://overpass-api.de/api/interpreter";
/// Public mirrors tried when the main instance is overloaded (comma-separated list).
pub const DEFAULT_MIRRORS: &str = "https://overpass.kumi.systems/api/interpreter";
/// The public Nominatim geocoder.
pub const NOMINATIM_URL: &str = "https://nominatim.openstreetmap.org";
/// Sent with every request; the OSM usage policies ask tools to identify themselves.
pub const USER_AGENT: &str = concat!(
    "atm-finder/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/paul-costa/atm-finder)"
);

/// How often a request is attempted before giving up (first try plus retries).
const DEFAULT_ATTEMPTS: u32 = 4;
/// Wait before the first retry; every further retry doubles it.
const DEFAULT_BACKOFF_MS: u64 = 5000;

/// A request that failed for good, after all retries.
#[derive(Debug, Clone)]
pub struct HttpError {
    /// Human-readable description, safe to print (keys already blanked).
    pub message: String,
    /// The HTTP status the server answered with, or `None` for network problems.
    pub status: Option<u16>,
    /// Further requests of the same kind are pointless, e.g. because an API key was rejected.
    pub fatal: bool,
}

impl HttpError {
    /// An error without an HTTP status (network problem, invalid response).
    pub fn new(message: impl Into<String>) -> Self {
        HttpError {
            message: message.into(),
            status: None,
            fatal: false,
        }
    }

    /// An error for an HTTP error status.
    pub fn with_status(message: impl Into<String>, status: u16) -> Self {
        HttpError {
            message: message.into(),
            status: Some(status),
            fatal: false,
        }
    }

    /// Whether trying again (later, or on a mirror) makes sense: overload, server errors and
    /// network problems do; client errors such as a bad query (400) do not.
    pub fn retryable(&self) -> bool {
        self.status.is_none_or(|s| s == 429 || s >= 500)
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HttpError {}

/// The Overpass endpoint to use when `--overpass-url` is not given: `ATM_FINDER_OVERPASS_URL`
/// or the main public instance.
pub fn default_overpass_url() -> String {
    get_env("ATM_FINDER_OVERPASS_URL")
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| DEFAULT_OVERPASS_URL.to_string())
}

/// Blank the values of the `key` and `email` query parameters so a URL can be logged.
pub fn redact(url: &str) -> String {
    let mut out = url.to_string();
    for parameter in ["key=", "email="] {
        let mut search_from = 0;
        while let Some(found) = out[search_from..].find(parameter) {
            let name_start = search_from + found;
            let value_start = name_start + parameter.len();
            let value_end = out[value_start..]
                .find('&')
                .map(|p| value_start + p)
                .unwrap_or(out.len());
            // Only a real parameter (at the start of the query or after `&`), not e.g. "monkey=".
            let is_parameter =
                name_start == 0 || matches!(out.as_bytes()[name_start - 1], b'?' | b'&');
            if is_parameter {
                out.replace_range(value_start..value_end, "***");
                search_from = value_start + "***".len();
            } else {
                search_from = value_end;
            }
        }
    }
    out
}

/// Reduce an error page to its visible text (Overpass answers errors as HTML) and cut it to
/// `limit` characters so a message stays readable.
pub fn strip_html(text: &str, limit: usize) -> String {
    let mut visible = String::new();
    let mut inside_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if !inside_tag => visible.push(ch),
            _ => {}
        }
    }
    let single_spaced = visible.split_whitespace().collect::<Vec<_>>().join(" ");
    single_spaced.chars().take(limit).collect()
}

/// Encode one value for an `application/x-www-form-urlencoded` body or a query string.
pub fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The host part of a URL, for short log lines ("overpass-api.de failed, trying ...").
fn host_of(url: &str) -> &str {
    let without_scheme = url.split("://").nth(1).unwrap_or(url);
    without_scheme.split('/').next().unwrap_or(url)
}

/// The first `max_chars` characters of a text (safe for multi-byte characters).
fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// What one attempt of a request came back with.
enum Attempt {
    /// A 2xx answer with its body.
    Success(String),
    /// An HTTP error status with the readable part of its body.
    ErrorStatus(u16, String),
    /// No answer at all (DNS, connection, timeout).
    Transport(String),
}

/// The HTTP client of the tool. One instance is shared by everything that talks to the network.
pub struct Http {
    agent: ureq::Agent,
    /// The Overpass instance tried first.
    pub overpass_url: String,
    /// Overpass instances tried in turn when the first one fails with a retryable error.
    pub mirrors: Vec<String>,
    /// E-mail sent to Nominatim as the `email` parameter, as its usage policy recommends.
    pub contact: Option<String>,
    /// Log every request and response size (`ATM_FINDER_DEBUG=1`).
    pub debug: bool,
    /// Attempts per request, retries included.
    pub retries: u32,
    /// Wait before the first retry in milliseconds; doubled for every further retry.
    pub backoff_ms: u64,
}

impl Http {
    /// A client for `overpass_url`. The mirrors come from `ATM_FINDER_OVERPASS_MIRRORS`, the
    /// contact from the argument or `ATM_FINDER_CONTACT`, debugging from `ATM_FINDER_DEBUG`.
    pub fn new(overpass_url: String, contact: Option<String>) -> Http {
        // Overpass may legitimately take many minutes for a country-wide query, hence the long
        // read timeout; the query itself carries its own server-side timeout.
        let agent = ureq::AgentBuilder::new()
            .user_agent(USER_AGENT)
            .timeout_connect(Duration::from_secs(30))
            .timeout_read(Duration::from_secs(1000))
            .timeout_write(Duration::from_secs(120))
            .build();
        let mirrors = get_env("ATM_FINDER_OVERPASS_MIRRORS")
            .unwrap_or_else(|| DEFAULT_MIRRORS.to_string())
            .split(',')
            .map(|url| url.trim().to_string())
            .filter(|url| !url.is_empty())
            .collect();
        let contact = contact.or_else(|| get_env("ATM_FINDER_CONTACT").filter(|s| !s.is_empty()));
        let debug = get_env("ATM_FINDER_DEBUG").is_some_and(|v| !v.is_empty());
        Http {
            agent,
            overpass_url,
            mirrors,
            contact,
            debug,
            retries: DEFAULT_ATTEMPTS,
            backoff_ms: DEFAULT_BACKOFF_MS,
        }
    }

    /// GET a JSON document.
    pub fn get_json(&self, url: &str) -> Result<serde_json::Value, HttpError> {
        self.request("GET", url, &[], None)
    }

    /// POST a form body (`a=1&b=2`) and parse the JSON answer.
    pub fn post_form(&self, url: &str, body: String) -> Result<serde_json::Value, HttpError> {
        let body = ("application/x-www-form-urlencoded", body);
        self.request("POST", url, &[], Some(body))
    }

    /// POST a JSON body with extra headers (e.g. `Authorization`) and parse the JSON answer.
    pub fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, HttpError> {
        let body = ("application/json", body.to_string());
        self.request("POST", url, headers, Some(body))
    }

    /// Send a request with retries and parse the JSON answer.
    ///
    /// `body` is `(content type, text)`. Retryable failures (see [`HttpError::retryable`]) are
    /// tried again after a growing pause; the last failure is returned when all attempts fail.
    fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<(&str, String)>,
    ) -> Result<serde_json::Value, HttpError> {
        if self.debug {
            let size = body
                .as_ref()
                .map(|(_, text)| format!(" ({} bytes)", text.len()))
                .unwrap_or_default();
            log(&format!("  > {method} {}{size}", redact(url)));
        }
        let attempts = self.retries.max(1);
        let mut last_error = HttpError::new("no attempt made");
        for attempt in 0..attempts {
            if attempt > 0 {
                sleep(Duration::from_millis(self.backoff_ms << (attempt - 1)));
            }
            let started = Instant::now();
            match self.attempt(method, url, headers, body.as_ref()) {
                Attempt::Success(text) => {
                    if self.debug {
                        let seconds = started.elapsed().as_secs_f64();
                        log(&format!("  < {} bytes in {seconds:.1}s", text.len()));
                    }
                    return serde_json::from_str(&text).map_err(|e| {
                        HttpError::new(format!("invalid JSON from {}: {e}", redact(url)))
                    });
                }
                Attempt::ErrorStatus(code, detail) => {
                    let message = format!("HTTP {code} for {}: {detail}", redact(url));
                    last_error = HttpError::with_status(message, code);
                    if !last_error.retryable() {
                        return Err(last_error);
                    }
                    let more_attempts_follow = attempt + 1 < attempts;
                    if more_attempts_follow {
                        let wait_seconds = (self.backoff_ms << attempt) / 1000;
                        log(&format!(
                            "  HTTP {code} from {} ({}), retrying in {wait_seconds}s",
                            host_of(url),
                            truncate_chars(&detail, 160)
                        ));
                    }
                }
                Attempt::Transport(detail) => {
                    last_error =
                        HttpError::new(format!("network error for {}: {detail}", redact(url)));
                }
            }
        }
        Err(last_error)
    }

    /// One attempt of a request, without retries.
    fn attempt(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&(&str, String)>,
    ) -> Attempt {
        let mut request = self
            .agent
            .request(method, url)
            .set("Accept", "application/json");
        for (name, value) in headers {
            request = request.set(name, value);
        }
        let result = match body {
            Some((content_type, text)) => {
                request.set("Content-Type", content_type).send_string(text)
            }
            None => request.call(),
        };
        match result {
            Ok(response) => match response.into_string() {
                Ok(text) => Attempt::Success(text),
                Err(e) => Attempt::Transport(format!("reading the response: {e}")),
            },
            Err(ureq::Error::Status(code, response)) => {
                let body = response.into_string().unwrap_or_default();
                Attempt::ErrorStatus(code, strip_html(&body, 1000))
            }
            Err(ureq::Error::Transport(transport)) => Attempt::Transport(transport.to_string()),
        }
    }

    /// The Overpass instances to try, in order: the configured one, then the mirrors.
    fn overpass_endpoints(&self) -> Vec<&str> {
        let mut endpoints = vec![self.overpass_url.as_str()];
        for mirror in &self.mirrors {
            if *mirror != self.overpass_url {
                endpoints.push(mirror.as_str());
            }
        }
        endpoints
    }

    /// Run an Overpass QL query and return the parsed JSON answer.
    ///
    /// Each endpoint gets the usual retries; when it still fails with a retryable error, the
    /// next mirror is tried. A non-retryable error (typically a query syntax error) is returned
    /// at once, because a mirror would answer the same.
    pub fn overpass(&self, query: &str) -> Result<serde_json::Value, HttpError> {
        if self.debug {
            log(&format!("    {}", query.replace('\n', "\n    ")));
        }
        let endpoints = self.overpass_endpoints();
        let body = format!("data={}", form_encode(query));
        let mut last_error = HttpError::new("no Overpass endpoint configured");
        for (index, endpoint) in endpoints.iter().enumerate() {
            let error = match self.post_form(endpoint, body.clone()) {
                Ok(answer) => return Ok(answer),
                Err(error) => error,
            };
            let next = endpoints.get(index + 1);
            match next {
                Some(next_endpoint) if error.retryable() => {
                    log(&format!(
                        "  {} failed ({}...), trying {}",
                        host_of(endpoint),
                        truncate_chars(&error.message, 100),
                        host_of(next_endpoint)
                    ));
                    last_error = error;
                }
                _ => return Err(error),
            }
        }
        Err(last_error)
    }

    /// The full URL of a Nominatim request, with the contact e-mail attached when configured.
    pub fn nominatim_url(&self, path: &str, params: &[(&str, String)]) -> String {
        let mut query: Vec<String> = params
            .iter()
            .map(|(name, value)| format!("{name}={}", form_encode(value)))
            .collect();
        if let Some(contact) = &self.contact {
            query.push(format!("email={}", form_encode(contact)));
        }
        format!("{NOMINATIM_URL}/{path}?{}", query.join("&"))
    }

    /// GET from Nominatim (`search`, `reverse`, ...) with the given query parameters.
    pub fn nominatim(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<serde_json::Value, HttpError> {
        self.get_json(&self.nominatim_url(path, params))
    }
}

#[cfg(test)]
pub mod testing {
    //! A throw-away HTTP server for the tests: answers canned responses, one per connection.
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    pub struct Server {
        /// `http://127.0.0.1:<port>`
        pub url: String,
        /// The raw requests received so far, in order.
        pub requests: Arc<Mutex<Vec<String>>>,
    }

    /// Read one HTTP request completely: the headers and, if `Content-Length` says so, the body.
    ///
    /// Reading everything matters on Windows: closing a socket that still holds unread bytes
    /// resets the connection, and the client never sees the response.
    fn read_request(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = stream.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..n]);
            let Some(header_end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&bytes[..header_end]).to_string();
            let content_length = head
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if bytes.len() >= header_end + 4 + content_length {
                break;
            }
        }
        String::from_utf8_lossy(&bytes).to_string()
    }

    /// Serve `responses` (status, body) in order; each accepted connection consumes one.
    pub fn serve(responses: Vec<(u16, &'static str)>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        std::thread::spawn(move || {
            for (status, body) in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                seen.lock().unwrap().push(read_request(&mut stream));
                let reason = if status == 200 { "OK" } else { "Error" };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.flush();
                let _ = stream.shutdown(Shutdown::Both);
            }
        });
        Server { url, requests }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client with fast retries for the tests.
    fn http(url: &str, mirrors: Vec<String>) -> Http {
        let mut client = Http::new(url.to_string(), None);
        client.mirrors = mirrors;
        client.backoff_ms = 10;
        client.retries = 3;
        client
    }

    #[test]
    fn redact_key_and_email() {
        assert_eq!(
            redact("https://x/search?q=Graz&email=me@example.org&key=AIzaSECRET&format=json"),
            "https://x/search?q=Graz&email=***&key=***&format=json"
        );
        assert_eq!(redact("https://x/?monkey=1"), "https://x/?monkey=1");
    }

    #[test]
    fn encode_and_strip() {
        assert_eq!(form_encode("a b&c=d/ö"), "a+b%26c%3Dd%2F%C3%B6");
        assert_eq!(
            strip_html("<html><p>busy <b>now</b></p>\n\n x</html>", 8),
            "busy now"
        );
    }

    #[test]
    fn retries_then_mirror() {
        let primary = testing::serve(vec![(502, "<p>at capacity</p>"), (503, "x"), (503, "x")]);
        let mirror = testing::serve(vec![(200, r#"{"elements":[1]}"#)]);
        let client = http(&primary.url, vec![mirror.url.clone()]);
        let answer = client.overpass("[out:json];").unwrap();
        assert_eq!(answer["elements"][0], 1);
        assert_eq!(primary.requests.lock().unwrap().len(), 3);
        assert!(mirror.requests.lock().unwrap()[0].contains("data=%5Bout%3Ajson%5D%3B"));
    }

    #[test]
    fn client_errors_are_not_retried() {
        let primary = testing::serve(vec![(400, "<p>parse error</p>"), (200, "{}")]);
        let client = http(&primary.url, vec![]);
        let error = client.overpass("bad").unwrap_err();
        assert_eq!(error.status, Some(400));
        assert!(error.message.contains("parse error"));
        assert_eq!(primary.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn nominatim_url_adds_contact() {
        let mut client = http("http://unused", vec![]);
        assert_eq!(
            client.nominatim_url("search", &[("q", "Graz".to_string())]),
            "https://nominatim.openstreetmap.org/search?q=Graz"
        );
        client.contact = Some("me@example.org".into());
        assert_eq!(
            client.nominatim_url("search", &[("q", "Graz".to_string())]),
            "https://nominatim.openstreetmap.org/search?q=Graz&email=me%40example.org"
        );
    }
}
