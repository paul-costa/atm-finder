#!/usr/bin/env bash
# Deploy the website on costaserver: test, build, smoke-test, then switch the served copy.
#
#   server/deploy.sh            build the current checkout and publish it
#   server/deploy.sh --export   first re-export data/places.txt from OSM (atm-finder batch;
#                               rarely, the public Overpass servers are run by volunteers)
#
# The server (atm-server.service) serves ~/.local/share/atm-finder/site, a symlink to the
# newest build in site-releases/; the last three builds are kept, so going back is one `ln -sfn`.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
STATE="${ATM_STATE_DIR:-$HOME/.local/share/atm-finder}"

cargo test -q --workspace
node --test server/quota.test.js >/dev/null
cargo build -q -p atm-finder --release
if [ "${1:-}" = "--export" ]; then
  ./target/release/atm-finder batch
fi
./web/build.sh >/dev/null
(cd web && [ -d node_modules/playwright ] || npm install --no-save --silent playwright; node e2e/smoke.js)

release="$STATE/site-releases/$(date +%Y%m%d-%H%M%S)-$(git rev-parse --short HEAD)"
mkdir -p "$release"
cp -a web/dist/. "$release/"
ln -sfn "$release" "$STATE/site.new" && mv -T "$STATE/site.new" "$STATE/site"
ls -1d "$STATE"/site-releases/* | head -n -3 | xargs -r rm -rf
(cd server && npm install --silent --omit=dev)
# The site is served from the symlink, so it is live now. Restart only for new server code,
# and never in the middle of creating a list.
if grep -q '"state":"running"' "$STATE/lock.json" 2>/dev/null; then
  echo "Eine Liste wird gerade erstellt: Server-Neustart später mit systemctl --user restart atm-server"
else
  systemctl --user restart atm-server
fi
echo "Live: $release"
