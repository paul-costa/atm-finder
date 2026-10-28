// Browser smoke test of the built site (web/dist) with Overpass and Nominatim mocked.
//
// It serves web/dist on a local port, opens the page in Chromium at a phone and a desktop size,
// searches for Graz, checks the result summary, the place in the page address, the map markers,
// the three downloads, the address completion, the language toggle and opening ?place=Graz. The Overpass answers are built from
// data/graz.csv, so no network is needed and the test is deterministic.
//
//   cd web && ./build.sh && npm install playwright && npx playwright install chromium && node e2e/smoke.js
//
// Set SCREENSHOTS=1 to keep screenshots next to this file.
const { chromium } = require('playwright');
const fs = require('fs');
const http = require('http');
const path = require('path');

const root = path.resolve(__dirname, '..');
const dist = path.join(root, 'dist');
const csvPath = path.resolve(root, '..', 'data', 'graz.csv');

// --- fixtures: Overpass-style elements from the first 40 rows of the Graz export ---------------
const rows = fs.readFileSync(csvPath, 'utf8').split('\n').filter(Boolean).slice(1).map((line) => {
  const cols = line.split(',');
  return { id: cols[0], lat: parseFloat(cols[1]), lon: parseFloat(cols[2]), name: cols[3], operator: cols[4], street: cols[6], postcode: cols[7], city: cols[8] };
}).filter((r) => !Number.isNaN(r.lat)).slice(0, 40);

const atmElements = rows.map((r, i) => ({
  type: 'node',
  id: parseInt(r.id.replace(/^osm:(node|way)\//, ''), 10) || i + 1,
  lat: r.lat,
  lon: r.lon,
  // every third record is a bank with an ATM, every second one comes without an address
  tags: Object.assign({ amenity: i % 3 === 0 ? 'bank' : 'atm', atm: 'yes', operator: r.operator, name: r.name },
    i % 2 === 0 ? { 'addr:street': r.street, 'addr:postcode': r.postcode, 'addr:city': r.city } : {}),
}));
const square = [[15.30, 46.98], [15.55, 46.98], [15.55, 47.15], [15.30, 47.15], [15.30, 46.98]];
const responses = {
  candidates: { elements: [{ type: 'relation', id: 34719, tags: { name: 'Graz', admin_level: '6', 'ref:at:gkz': '60101', boundary: 'administrative' } }] },
  geometry: { elements: [
    { type: 'relation', id: 34719, tags: { name: 'Graz', admin_level: '6' }, members: [{ type: 'way', ref: 1, role: 'outer' }] },
    { type: 'way', id: 1, geometry: square.map(([lon, lat]) => ({ lon, lat })) }] },
  atms: { elements: atmElements },
  nearby: { elements: atmElements.filter((e, i) => i % 2 === 1).slice(0, 10).map((e) => ({
    type: 'node', id: 900000 + e.id, lat: e.lat + 0.0001, lon: e.lon,
    tags: { 'addr:street': 'Teststraße', 'addr:housenumber': String(e.id % 90 + 1), 'addr:postcode': '8010', 'addr:city': 'Graz' } })) },
};

function classify(postData) {
  const body = decodeURIComponent((postData || '').replace(/^data=/, '').replace(/\+/g, ' '));
  if (body.includes('"boundary"="administrative"')) return 'candidates';
  if (body.includes('way(r.r)')) return 'geometry';
  if (body.includes('"amenity"="atm"')) return 'atms';
  if (body.includes('addr:housenumber"](around')) return 'nearby';
  return 'unknown';
}

// --- a tiny static file server for web/dist --------------------------------------------------
const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.wasm': 'application/wasm', '.json': 'application/json', '.png': 'image/png' };
function serve() {
  return new Promise((resolve) => {
    const server = http.createServer((req, res) => {
      const file = path.join(dist, decodeURIComponent(req.url.split('?')[0]).replace(/\/$/, '/index.html'));
      fs.readFile(file, (err, data) => {
        if (err) { res.writeHead(404); res.end(); return; }
        res.writeHead(200, { 'Content-Type': types[path.extname(file)] || 'application/octet-stream' });
        res.end(data);
      });
    });
    server.listen(0, '127.0.0.1', () => resolve({ server, url: `http://127.0.0.1:${server.address().port}` }));
  });
}

function assert(condition, message) {
  if (!condition) throw new Error(`assertion failed: ${message}`);
}

(async () => {
  if (!fs.existsSync(path.join(dist, 'pkg', 'atm_finder_web_bg.wasm'))) throw new Error('web/dist is missing; run ./build.sh first');
  const { server, url } = await serve();
  const browser = await chromium.launch();
  const report = [];
  try {
    for (const [label, viewport] of [['mobile', { width: 390, height: 844 }], ['desktop', { width: 1100, height: 900 }]]) {
      const context = await browser.newContext({ viewport, locale: 'de-AT', acceptDownloads: true });
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      const calls = { overpass: [], nominatim: 0 };
      let atmCalls = 0;
      await page.route(/overpass/, async (route) => {
        const kind = classify(route.request().postData());
        calls.overpass.push(kind);
        // the first ATM query on the phone run answers 502 so the retry path is exercised
        if (kind === 'atms' && atmCalls++ === 0 && label === 'mobile') {
          return route.fulfill({ status: 502, contentType: 'text/html', body: '<html><body>502 at capacity</body></html>' });
        }
        await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(responses[kind] || { elements: [] }) });
      });
      await page.route(/nominatim/, async (route) => {
        calls.nominatim += 1;
        await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ address: { road: 'Reversegasse', house_number: '7', postcode: '8020', city: 'Graz' } }) });
      });
      await page.route(/tile\.openstreetmap\.org/, (route) => route.abort());
      // The list server: first Graz has a list, later (lists = []) it can be created.
      let lists = [{ slug: 'graz', relation: 34719, url: 'https://maps.app.goo.gl/test', in_list: 36, total: 38 }];
      const free = { state: 'free', place: '', relation: 0, until: '', error: '' };
      await page.route(/\/api\/lists/, async (route) => {
        const running = { state: 'running', place: 'Graz', relation: 34719, until: '', error: '' };
        const body = route.request().method() === 'POST' ? { lists, lock: running } : { lists, lock: free };
        await route.fulfill({ status: route.request().method() === 'POST' ? 202 : 200, contentType: 'application/json', body: JSON.stringify(body) });
      });

      await page.goto(`${url}/index.html`, { waitUntil: 'networkidle' });
      assert((await page.title()) === 'Bankomaten in Österreich', 'German title from the browser locale');
      assert((await page.locator('#ready-list li').count()) >= 1, 'ready-made files are listed');
      const readyHref = await page.locator('#ready-list a', { hasText: 'Bezirk Graz-Umgebung' }).getAttribute('href');
      assert(readyHref === '?place=Bezirk+Graz-Umgebung&level=district', `ready-made link: ${readyHref}`);
      assert((await page.locator('#level-select').count()) === 0, 'no level select');
      assert((await page.locator('#ready-list a.gmaps-link').count()) === 1, 'Google Maps link next to the Graz files');

      await page.fill('#place-input', 'Graz');
      await page.click('#search-button');
      await page.waitForSelector('#downloads:not([hidden])', { timeout: 60000 });
      const summary = await page.textContent('#result-summary');
      assert(summary.startsWith('38 Bankomaten in Graz (40 Rohtreffer'), `summary: ${summary}`);
      assert((await page.textContent('#list-count')) === '38', 'list count');
      const placeLine = await page.textContent('#result-place');
      assert(placeLine === 'Graz · Stadt · Steiermark', `place line: ${placeLine}`);
      assert(new URL(page.url()).search === '?place=Graz', `page address: ${page.url()}`);
      assert((await page.getAttribute('#gmaps-list-link', 'href')) === 'https://maps.app.goo.gl/test', 'list link in the result card');
      assert((await page.textContent('#gmaps-note')).startsWith('36 von 38 Bankomaten'), 'partial list note');
      await page.waitForTimeout(300);
      const mapBox = await page.evaluate(() => document.getElementById('map').getBoundingClientRect());
      const visible = await page.locator('#map path.leaflet-interactive').evaluateAll((els, box) => els.filter((e) => {
        const r = e.getBoundingClientRect();
        return r.right > box.left && r.left < box.right && r.bottom > box.top && r.top < box.bottom;
      }).length, { left: mapBox.left, right: mapBox.right, top: mapBox.top, bottom: mapBox.bottom });
      assert(visible === 39, `markers and boundary inside the map viewport: ${visible}`);

      for (const [button, check] of [['#dl-csv', (t) => t.startsWith('id,lat,lon,') && t.trim().split('\n').length === 39], ['#dl-geojson', (t) => JSON.parse(t).features.length === 39], ['#dl-kml', (t) => (t.match(/<Placemark>/g) || []).length === 38]]) {
        const [download] = await Promise.all([page.waitForEvent('download'), page.click(button)]);
        const text = fs.readFileSync(await download.path(), 'utf8');
        assert(check(text), `download ${button} (${download.suggestedFilename()})`);
      }

      await page.click('#fill-button');
      await page.waitForFunction(() => document.getElementById('fill-button').hidden, null, { timeout: 120000 });
      const note = await page.textContent('#address-note');
      assert(note === 'Adressen ergänzt: 9 über nächste Hausnummer, 9 per Rückwärtssuche, 0 offen.', `address note: ${note}`);
      assert(calls.nominatim === 9, `nominatim calls: ${calls.nominatim}`);

      await page.click('#lang-toggle');
      assert((await page.title()) === 'ATMs in Austria', 'English title after toggle');
      assert((await page.textContent('#result-summary')).startsWith('38 ATMs in Graz'), 'English summary');

      // The address with ?place= runs the search by itself (in English now, the stored choice).
      await page.goto(`${url}/index.html?place=Graz`, { waitUntil: 'networkidle' });
      await page.waitForSelector('#downloads:not([hidden])', { timeout: 60000 });
      assert((await page.inputValue('#place-input')) === 'Graz', 'place input filled from the address');
      assert((await page.textContent('#result-place')) === 'Graz · city · Steiermark', 'English place line');
      lists = [];
      await page.goto(`${url}/index.html?place=Graz`, { waitUntil: 'networkidle' });
      await page.waitForSelector('#gmaps-create:not([hidden]):not([disabled])', { timeout: 60000 });
      await page.click('#gmaps-create');
      await page.waitForFunction(() => document.getElementById('gmaps-note').textContent.startsWith('The list is being created'), null, { timeout: 10000 });
      assert(await page.isDisabled('#gmaps-create'), 'create button locked while running');
      if (process.env.SCREENSHOTS) await page.screenshot({ path: path.join(__dirname, `screenshot-${label}.png`), fullPage: true });
      assert(errors.length === 0, `page errors: ${errors.join('; ')}`);
      report.push(`${label}: ok (overpass calls ${calls.overpass.join(',')}; nominatim ${calls.nominatim})`);
      await context.close();
    }
  } finally {
    await browser.close();
    server.close();
  }
  console.log(report.join('\n'));
})().catch((e) => {
  console.error('SMOKE TEST FAILED:', e.message);
  process.exit(1);
});
