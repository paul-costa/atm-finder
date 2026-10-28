# atm-finder

[![CI](https://github.com/paul-costa/atm-finder/actions/workflows/ci.yml/badge.svg)](https://github.com/paul-costa/atm-finder/actions/workflows/ci.yml)

Export the ATM locations of any place in Austria (a municipality, a district, a state or the whole
country) from [OpenStreetMap](https://www.openstreetmap.org) as CSV, GeoJSON or KML. A single
Rust binary, no API key needed; the same engine also runs in the browser.

```bash
atm-finder export Graz --fill-addresses -o graz.csv -o graz.geojson -o graz.kml
```

```
Resolving 'Graz' ...
  Graz (city, Steiermark, GKZ 60101, relation 34719)
Source osm for Graz ...
  178 records in 44.8s
  150 ATMs in Graz (178 raw records)
Filling addresses for 69 records: nearest OSM house number (<= 50 m) first ...
  addresses: 68 from nearby house numbers, 1 reverse geocoded, 0 unresolved
Wrote 150 ATMs as csv -> graz.csv
Wrote 150 ATMs as geojson -> graz.geojson
Wrote 150 ATMs as kml -> graz.kml
```

## Website: [atm-finder.costapaul.com](https://atm-finder.costapaul.com)

The same engine runs in the browser as WebAssembly ([`web/`](web/README.md)): type a place, get
the map, the list and the downloads, no installation. The level (municipality, district, state)
is picked automatically, and `?place=Graz` opens a place directly.

- **Ready-made files:** CSV, GeoJSON and KML for the places in [`data/places.txt`](data/places.txt)
  (Graz, Graz-Umgebung, Leoben and the state capitals), each with a link to its public
  **Google Maps list**.
- **Google Maps list for any place:** a search result without a list offers "Create Google Maps
  list". The server matches the ATMs to Google places and saves them into a new public list;
  one list at a time, then a 30-minute pause for everybody. Every list is backed up on the
  server and can be recreated if the Google account is lost
  ([`server/`](server/README.md)).

The site and its list API run on a home server (costaserver) behind a Cloudflare Tunnel; deploys
happen there with [`server/deploy.sh`](server/deploy.sh), not through GitHub.

## Installation

Download the binary for Linux, macOS or Windows from the
[releases](https://github.com/paul-costa/atm-finder/releases), unpack it and put `atm-finder` on
your `PATH`. With a Rust toolchain installed you can build it yourself instead:

```bash
cargo install --git https://github.com/paul-costa/atm-finder atm-finder
```

## Command line

```bash
atm-finder export Graz -o graz.csv                 # a city
atm-finder export Leoben -o leoben.geojson         # a town; any municipality works
atm-finder export Graz-Umgebung --level district -o gu.csv
atm-finder export Steiermark -o steiermark.kml     # a whole state (a minute or two)
atm-finder export Austria -o austria.csv           # the whole country (several minutes)
atm-finder export Graz Leoben Weiz -o three.csv    # several places; the `place` column tells them apart
atm-finder export r123456 -o place.csv             # by OSM relation id (see `atm-finder places`)
atm-finder export Graz --fill-addresses --boundary-out graz-boundary.geojson -o graz.csv
atm-finder export Graz                             # CSV to stdout
atm-finder places Neukirchen                       # which boundaries carry this name?
```

### How a place name is resolved

The name is matched case-insensitively against the `name` of the administrative boundaries in
Austria. If nothing matches exactly, a name that differs only by a prefix such as `Bezirk ` or by
`St.`/`Sankt` is accepted (`Graz-Umgebung` finds `Bezirk Graz-Umgebung`); otherwise the tool lists
the boundaries whose name contains your text and stops. `atm-finder places TEXT` shows that list
directly.

| `--level`      | OSM `admin_level` | Examples                                   |
|----------------|-------------------|--------------------------------------------|
| `state`        | 4                 | Steiermark, Wien, Salzburg (the state)     |
| `district`     | 6                 | Graz-Umgebung, Leoben (the district), Graz |
| `municipality` | 8                 | Leoben (the town), Seiersberg-Pirka        |
| `borough`      | 9                 | Innere Stadt (Vienna/Graz city districts)  |

When several boundaries share a name, the municipality wins, then district, state and borough.
`Salzburg` therefore means the city; `--level state` gives the state. If two boundaries at the
chosen level share the name (two villages called Neukirchen), the tool lists them with their state
and exits; pass `--level`, or the relation id (`r123456`) shown in the list.

The boundary polygon comes from the same OSM relation, so results are clipped to the exact
administrative area, including exclaves and holes.

### Output formats

The format follows the file extension; `--format` overrides it. `-o` can be repeated.

| Extension           | Content                                                                 |
|---------------------|-------------------------------------------------------------------------|
| `.csv`              | one row per ATM, UTF-8, comma-separated, columns below                  |
| `.geojson`, `.json` | FeatureCollection of points with the same columns as properties         |
| `.kml`              | placemarks with name, description and all columns as ExtendedData       |

**Google My Maps:** open [mymaps.google.com](https://www.google.com/mymaps) → *Create a new map* →
*Import* → choose the `.kml` file. Each ATM becomes a pin labelled with its operator; the columns
appear in the pin's details. Google Maps' *Saved lists* have no import; the website's lists are
made by the server (see above).

### Columns

| Column            | Content                                                                                           |
|-------------------|---------------------------------------------------------------------------------------------------|
| `id`              | stable identifier of the primary record, e.g. `osm:node/2345275519` (survives re-runs)            |
| `lat`, `lon`      | WGS84 coordinates                                                                                 |
| `name`, `operator`, `brand` | name, operator (bank) and brand as tagged in the source                                 |
| `street`, `postcode`, `city` | address                                                                                |
| `address_source`  | `source` = came with the record; `osm_nearby` = nearest OSM house number within 50 m; `nominatim_reverse` / `google_reverse` = reverse geocoded (often just the street) |
| `kind`            | `atm` (separately mapped machine), `bank_with_atm` (branch with ATM), `poi_with_atm` (e.g. supermarket with ATM), `branch_with_atm` (bank branch finder), `web_listing` (firecrawl) |
| `opening_hours`, `fee`, `cash_in`, `wheelchair`, `indoor`, `check_date` | OSM tags as-is (`opening_hours` in OSM syntax or `24/7`) |
| `place`           | the place the row was exported for                                                                |
| `sources`, `source_ids` | which sources know this ATM, with their ids (`osm:node/123;bankaustria:456`)                 |
| `url`             | link to the source record                                                                         |

### Options (`atm-finder export`)

| Option                  | Meaning                                                                                              |
|-------------------------|------------------------------------------------------------------------------------------------------|
| `--level`               | `state`, `district`, `municipality` or `borough` (default: automatic, see above)                      |
| `--sources`             | comma-separated `osm`, `bankaustria`, `firecrawl` (default `osm`)                                    |
| `-o`, `--format`        | output file(s) and format, see above                                                                  |
| `--boundary-out FILE`   | write the resolved boundary polygon(s) as GeoJSON                                                     |
| `--fill-addresses`      | complete missing addresses (see below)                                                                |
| `--geocoder`            | `nominatim` (default, free) or `google` for `--fill-addresses` and firecrawl addresses               |
| `--country-code`        | ISO code passed to the geocoder (default `at`)                                                        |
| `--merge-radius M`      | metres within which records of the same operator count as one ATM (default 40)                       |
| `--no-merge`            | keep the raw records of all sources                                                                   |
| `--contact EMAIL`       | e-mail sent with Nominatim requests, as the [usage policy](https://operations.osmfoundation.org/policies/nominatim/) asks (or `ATM_FINDER_CONTACT`) |
| `--overpass-url URL`    | another Overpass instance (or `ATM_FINDER_OVERPASS_URL`); mirrors tried on overload: `ATM_FINDER_OVERPASS_MIRRORS` |
| `-q`                    | no progress output                                                                                    |

### Address completion (`--fill-addresses`)

Roughly half of the ATMs in OSM carry no address. With `--fill-addresses` the tool looks for the
nearest OSM object with a house number within 50 m (one Overpass query per 150 ATMs, real street
addresses preferred over market stalls and the like) and only reverse-geocodes the rest through
Nominatim (1 request per second) or Google. Measured for Graz on 2026-10-27 against Google's
geocoder: median deviation 21 m, 78 % of the completed addresses within 50 m (plain reverse
geocoding: 54 m / 47 %).

### Sources

| Source        | What                                                                  | Needs                       |
|---------------|-----------------------------------------------------------------------|-----------------------------|
| `osm`         | `amenity=atm`, `amenity=bank` + `atm=yes`, other POIs with `atm=yes` via the Overpass API | nothing (ODbL)  |
| `bankaustria` | branches with cash withdrawal from the UniCredit Bank Austria branch finder, clipped to the boundary | nothing |
| `firecrawl`   | web search + LLM extraction of listed addresses, geocoded afterwards. Rarely adds anything; useful for one known listing page via `--firecrawl-url` | `FIRECRAWL_API_KEY` |

Records from different sources within `--merge-radius` metres are merged when their operators
match ("Die Steiermärkische" = "Sparkasse") or one of them is unknown. Two separately mapped OSM
ATMs are always two machines.

## Geocoding an address list

`atm-finder geocode` adds coordinates to any CSV of addresses (the second half of this project).
Address columns are detected (`address`/`Adresse`, or street, house number, postcode, city in
English or German), `,` `;` and tab delimiters are kept, rows that already have `lat`/`lon` are
skipped and identical addresses are looked up once.

```bash
atm-finder geocode examples/addresses.csv -o with-coordinates.csv                 # Nominatim, free
GOOGLE_MAPS_API_KEY=AIza... atm-finder geocode addresses.csv --geocoder google -o out.csv
atm-finder geocode list.csv --address-column Street --address-column Town --encoding cp1252   # old Excel exports
```

New columns: `lat`, `lon`, `geo_address` (what was found), `geo_accuracy` (Google: `ROOFTOP`,
`RANGE_INTERPOLATED`, `GEOMETRIC_CENTER`, `APPROXIMATE`; Nominatim: the OSM object type),
`geo_partial_match` (`yes` = check it), `geo_status` (`ok`, `not found`, `existing`, `no address`,
`error: ...`), `geo_source`.

## Google Geocoding API (optional)

Create a key in the [Google Cloud Console](https://console.cloud.google.com/google/maps-apis/credentials)
with the *Geocoding API* enabled and restrict it to that API. Provide it as the environment
variable `GOOGLE_MAPS_API_KEY` or in a `.env` file in the working directory
(`GOOGLE_MAPS_API_KEY=AIza...`). 10,000 requests per month are free, then USD 5 per 1,000.

Google's [terms](https://cloud.google.com/maps-platform/terms/maps-service-terms) allow caching
geocoding results for at most 30 days and forbid showing them on non-Google maps. For files you
keep or publish, stay with Nominatim/OSM (the default).

If you used the earlier Python prototype on macOS, where the key lived in the keychain, export it like this:

```bash
export GOOGLE_MAPS_API_KEY="$(security find-generic-password -s bankomaten-google-maps -w)"
```

## Fair use of the OSM servers

The public [Overpass API](https://wiki.openstreetmap.org/wiki/Overpass_API) and
[Nominatim](https://operations.osmfoundation.org/policies/nominatim/) are run by volunteers. The
tool identifies itself with a User-Agent, sends at most one Nominatim request per second, batches
Overpass queries, retries with exponential backoff (5, 10, 20 s) when a server answers 429/5xx and
then falls back to the public mirror `overpass.kumi.systems` (configurable via
`ATM_FINDER_OVERPASS_MIRRORS`). Please do not loop over all 2,000 municipalities
every night; export the state or the country instead (one query), and pass `--contact` for larger
runs. For heavy use, run your own Overpass instance and point `--overpass-url` at it.

## Data sources considered

Tested for Graz on 2026-10-27; the Mastercard ATM locator served as a completeness reference
(173 machines within 8 km of the main square, slightly beyond the city limits).

| Approach | Result for Graz | Cost | Legal | In the tool |
|---|---|---|---|---|
| **OpenStreetMap (Overpass)** | 178 raw records → 150 ATMs inside the city; 166 within the 8 km circle vs 173 Mastercard machines; 8 of 12 Mastercard machines around the main square present within 60 m | free, no key, 30-60 s | ODbL: attribution, share-alike for derived databases | yes, default |
| **Bank branch finders** | Bank Austria: open JSON, branches only → 2 in Graz. Sparkasse: map behind a cookie wall and bot manager → not automatable | free | each bank's terms; one fragile adapter per bank | Bank Austria |
| **Card network locators** (Mastercard, Visa websites) | most complete source: terminal id, operator, availability | free | website blocked for scripts (Akamai, HTTP 403); legal route is the [Mastercard Locations API](https://developer.mastercard.com/locations/documentation/) (developer account, paid plan) | no |
| **Firecrawl** (search + scrape + JSON extraction) | addresses at best, which must be geocoded; LLMs may invent coordinates | ~100 credits per run | terms of the scraped pages | optional |
| **Google Places API** | good coverage but 20 results per request → grid search needed | paid, key | results may be cached for 30 days only → no permanent export | no |
| **Google Maps scrapers** (Apify, SerpAPI, …) | coordinates from Google Maps | paid | violates Google's terms | no |

OpenStreetMap is the best base: structured, real coordinates, free to use and republish. Its
coverage in Austria is good but not complete; the typical gaps are second machines at one
location and some ATMs of large branches. The fix is to survey them and add them to
OpenStreetMap, which improves this tool and everything else built on OSM.

## Development

A Cargo workspace with three crates, plus the server:

| Crate | What |
|---|---|
| `crates/core` (`atm-finder-core`) | the engine: Overpass queries and parsers, Austrian boundaries, merging, address matching, CSV/GeoJSON/KML writers. No I/O, so it runs natively and in WebAssembly. |
| `crates/cli` (`atm-finder`) | the command-line tool: blocking HTTP with retries and the Overpass mirror, Nominatim and Google geocoders, the commands above plus `batch` and `data-index` for the website's ready-made files. |
| `web` (`atm-finder-web`) | the website, see [`web/README.md`](web/README.md). |
| `server/` (Node) | serves the site and the Google Maps list API on costaserver, see [`server/README.md`](server/README.md). |

```bash
git clone https://github.com/paul-costa/atm-finder && cd atm-finder
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
cargo run -p atm-finder -- export Graz -o graz.csv
```

Tests run without network: the Overpass responses are small fixtures, and the HTTP layer is
tested against a local throw-away server. The website has a browser smoke test
(`web/e2e/smoke.js`, Playwright) that mocks Overpass, Nominatim and the list API. On GitHub, CI
runs format, lint and tests on Linux, macOS and Windows, and a tag `vX.Y.Z` publishes prebuilt
binaries through the *Release* workflow. Nothing is deployed from GitHub.

`atm-finder batch` exports the places listed in [`data/places.txt`](data/places.txt) (`Name` or
`Name|level` per line) into `data/`; on costaserver `server/deploy.sh --export` does that before
building the site.

## Licence

Code: [MIT](LICENSE). Exported data: © [OpenStreetMap contributors](https://www.openstreetmap.org/copyright),
[ODbL](https://opendatacommons.org/licenses/odbl/); keep the attribution when you republish it.
