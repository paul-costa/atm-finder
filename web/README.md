# atm-finder web

The browser version of atm-finder: a Rust program compiled to WebAssembly that resolves an
Austrian place, queries the Overpass API, merges duplicates, completes addresses and offers the
result as a map and as CSV/GeoJSON/KML downloads. The browser talks to `overpass-api.de` (with
`overpass.kumi.systems` as fallback) and `nominatim.openstreetmap.org` directly; the only server
part is the Google Maps list API next to the site (see [`../server/`](../server/README.md)).
German and English (follows the browser language, switchable).

```
web/
  src/            engine.rs (browser HTTP, list API client), ui.rs (DOM), i18n.rs; the engine itself is crates/core
  static/         index.html, style.css, map.js (Leaflet glue + download helper), gmaps.svg, vendored Leaflet 1.9.4
  build.sh        cargo build (wasm32, profile "wasm") + wasm-bindgen + static files + data/ -> dist/
  e2e/smoke.js    browser smoke test
```

## Build locally

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version "$(grep -A1 'name = "wasm-bindgen"$' Cargo.lock | grep version | sed 's/.*"\(.*\)"/\1/')"
cargo test -p atm-finder-web -p atm-finder-core && ./web/build.sh
python3 -m http.server --directory web/dist 8000   # http://localhost:8000 (without the list API)
```

`wasm-opt` (binaryen) is used automatically when installed and shrinks the module further.
Without the list API (as above) the page simply shows no Google Maps lists.

## Browser smoke test

`e2e/smoke.js` opens the built site in Chromium (phone and desktop size), mocks Overpass,
Nominatim and the list API with fixtures built from `data/graz.csv`, and checks the search, the
map markers, the three downloads, the address completion, the language toggle, the Google Maps
list link and the "create list" button with its lock:

```bash
./build.sh && npm install playwright && npx playwright install chromium && node e2e/smoke.js
```

## Search

The level of a place (municipality, district, state, borough) is always picked automatically.
When a name matches several boundaries, the page shows them as buttons; the chosen one goes into
the address as `?place=Name&level=district`, which narrows the lookup when the link is opened.

## Data

`build.sh` runs `atm-finder data-index`, which copies the ready-made exports from `../data/` into
`dist/data/` and writes `dist/data/index.json`, which the page lists under "Ready-made files".

## Google Maps lists

The page reads `api/lists` from the server. Every ready-made place and every search result with a
list gets a link with the Google Maps logo to the public list. A place without one shows
"Create Google Maps list"; the server creates it (a few minutes), while everybody else sees the
button locked until the run plus a 30-minute cooldown is over. The page polls every 10 s while a
list is being created.
