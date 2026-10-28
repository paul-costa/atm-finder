# atm-finder server (costaserver)

The website [atm-finder.costapaul.com](https://atm-finder.costapaul.com) runs on costaserver:
a small Node server serves the built site and the Google Maps list API, a Cloudflare Tunnel makes
it public, and a Chromium in a container holds the Google account the lists belong to. Everything
runs as user services, without root.

```
server/
  atm-server.js   website (web/dist via the symlink ~/.local/share/atm-finder/site) + /api/lists,
                  lock and cooldown; commands serve | create-ready | restore
  lists.js        one list end to end: ATMs, Google place ids, Google Maps, share link, backup
  deploy.sh       test, build, smoke-test and publish the site (optionally re-export data/)
  systemd/        atm-server, atm-tunnel, atm-backup (+ timer)
```

| Part | Where |
|---|---|
| Site + API | `atm-server.service`, `127.0.0.1:8090` |
| Public HTTPS | `atm-tunnel.service`: Cloudflare Tunnel `atm-finder` → `127.0.0.1:8090`, config `~/.cloudflared/atm-finder.yml`; `atm.costapaul.com` (old name) is redirected to `atm-finder.costapaul.com` |
| Google account | container `atm-chromium` (linuxserver/chromium), profile in `~/.local/share/atm-finder/chromium`, remote view `https://costaserver.<tailnet>.ts.net:3011` (user `paul`, password in `vnc-pass`), DevTools on `127.0.0.1:9222` |
| Places API key | `~/.local/share/atm-finder/places-key` (mode 600) |
| State | `~/.local/share/atm-finder/`: `lists/*.json` (backups), `lock.json`, `site-releases/` |
| Backup copy | `/mnt/backup/atm-finder/lists/` at once after each list, plus `atm-backup.timer` daily |

## Creating a list

`POST /api/lists {relation, name}` (the "Create Google Maps list" button) takes the lock and:

1. exports the place from OpenStreetMap with the CLI (`atm-finder export r<relation>`);
2. looks up a Google place id per ATM with the Places API (New) Text Search: "Bankomat", type
   `atm`, restricted to a box of about ±55 m around the OSM position, field mask `places.id` only
   (the *Text Search Essentials (IDs Only)* SKU, free without limit);
3. opens Google Maps' shortlist page (`map_action=shortlist`, 20 place ids per page) in the
   account's Chromium, selects all and saves them into a new list "Bankomaten <place>";
4. shares the list and stores its public link.

ATMs that Google does not know as places are left out; the page says "n of total are in the
list", and CSV/KML still have all of them. Only one list is created at a time; after each run
(also a failed one) creating is locked for 30 minutes for everybody. Existing lists stay
available all the time.

`node server/atm-server.js create-ready` creates the lists for the ready-made places in `data/`
that do not have one yet (the cooldown does not apply).

## Backup and restore

Each list is stored completely in `lists/<slug>.json`: place, OSM relation, list name, share
link, date, account, and every ATM with OSM id, coordinates, label, address and Google place id.

Driving Google Maps by script is against Google's terms, so the account may be blocked one day,
and with it all lists and links. Then:

1. log in with another Google account in the Chromium (remote view above);
2. `systemctl --user stop atm-server && node server/atm-server.js restore && systemctl --user start atm-server`

`restore` recreates every list from its backup (no new Places API calls), keeps the old backups in
`lists-old/<date>/`, and the site shows the new links right away.

## Deploy

Deployments happen on costaserver, not on GitHub:

```bash
cd ~/atm-finder && git pull && server/deploy.sh             # code or data changed
server/deploy.sh --export                                   # re-export data/places.txt first (rarely)
```

Toolchain without root: rustup (+ `wasm32-unknown-unknown`, clippy, rustfmt), `cc` = `zig cc`
wrapper in `~/.local/bin`, `wasm-bindgen-cli` matching `Cargo.lock`, binaryen's `wasm-opt`, Node
LTS in `~/.local/node`, `cloudflared` in `~/.local/bin`.

## Setup from scratch

```bash
# Chromium for the Google account (log in once via the remote view)
podman run -d --name atm-chromium --restart unless-stopped --shm-size 1g --network host \
  -e PUID=1000 -e PGID=1000 -e TZ=Europe/Vienna -e CUSTOM_USER=paul -e PASSWORD="$(cat ~/.local/share/atm-finder/vnc-pass)" \
  -e CUSTOM_PORT=3010 -e CUSTOM_HTTPS_PORT=3011 \
  -e CHROME_CLI="--remote-debugging-port=9222 --remote-allow-origins=http://127.0.0.1 --lang=en-US https://www.google.com/maps" \
  -v ~/.local/share/atm-finder/chromium:/config lscr.io/linuxserver/chromium:latest

# Cloudflare Tunnel (costapaul.com is on Cloudflare DNS)
cloudflared tunnel login
cloudflared tunnel create atm-finder
cloudflared tunnel route dns atm-finder atm-finder.costapaul.com
cloudflared tunnel route dns atm-finder atm.costapaul.com      # old name, the server redirects it

# services
ln -sf ~/atm-finder/server/systemd/atm-* ~/.config/systemd/user/
systemctl --user daemon-reload && systemctl --user enable --now atm-server atm-tunnel atm-backup.timer
server/deploy.sh && node server/atm-server.js create-ready
```
