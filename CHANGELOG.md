# Changelog

## 1.1.0 (2026-10-29)

- The website moves to atm-finder.costapaul.com and runs on costaserver together with a small
  server (`server/`): deploys happen there (`server/deploy.sh`), no longer on GitHub. The
  Firebase setup and the Web and Export data workflows are gone.
- Public Google Maps lists: every ready-made place links its list with the Google Maps logo, and
  "Create Google Maps list" makes one for any searched place (one at a time, then a 30-minute
  pause for everybody). Lists are backed up on the server and can be recreated
  (`node server/atm-server.js restore`).
- The level select is gone; the level is always picked automatically.
- Website: a place can be opened by address (`?place=Graz`, `&level=district` when needed);
  the address follows every search, "Copy link" puts it on the clipboard, and the names in
  "Ready-made files" show the place on the map.
- Statutory cities (level 6 with a five-digit GKZ, such as Graz) are labelled "city"/„Stadt“
  instead of "district", on the website and in log lines.
- Place search treats `Sankt`, `St.` and `St` alike (OSM has both `St. Pölten` and
  `Sankt Veit …`); when a city and its district share a name, the exact spelling decides.
- `data/places.txt` now includes the state capitals.
- `web/build.sh` passes the WebAssembly features to `wasm-opt`, so the deployed module is about 10 % smaller.

## 1.0.0 (2026-10-28)

First public release.

- `atm-finder` command-line tool (Rust, single binary): exports the ATMs of any Austrian
  municipality, district, state or the whole country from OpenStreetMap as CSV, GeoJSON or KML;
  `places` to look up boundaries; `geocode` to add coordinates to address lists (Nominatim or
  Google); `batch` and `data-index` for the workflows.
- Website (`web/`): the same engine compiled to WebAssembly, with a map, downloads, address
  completion and a German/English interface; deployed to Firebase Hosting.
- Ready-made exports in `data/`, refreshed by the *Export data* workflow.
- CI on Linux, macOS and Windows; release binaries on `v*` tags.

The project grew out of a Python script for Graz written in October 2026. That prototype is not
published; its ideas (merging rules, address completion via the nearest house number, source
comparison) live on here.
