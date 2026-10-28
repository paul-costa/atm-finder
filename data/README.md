# Exported data

Files in this folder are produced by `atm-finder batch` from the places listed in
[`places.txt`](places.txt) (one per line, `Name` or `Name|level`); on costaserver
`server/deploy.sh --export` runs it and publishes the result. Regenerate locally with

```bash
atm-finder batch                           # everything in places.txt -> data/
atm-finder export Linz --fill-addresses -o data/linz.csv -o data/linz.geojson -o data/linz.kml
```

Each place has a `.csv`, a `.geojson` (GitHub shows it as a map) and a `.kml` (import into
Google My Maps). Column descriptions are in the main [README](../README.md#columns).

Data © [OpenStreetMap contributors](https://www.openstreetmap.org/copyright), licensed under the
[Open Database License (ODbL)](https://opendatacommons.org/licenses/odbl/). Keep this attribution
when you republish or build on the files.
