// Google Maps lists for atm-finder: match the ATMs of a place to Google places, put them into a
// new public list of the server's Google account, and keep a full backup of every list.
//
// Steps for one place (createList):
//   1. ATMs from the atm-finder CLI (`atm-finder export r<relation>`), or from data/<slug>.geojson.
//   2. A Google place id per ATM: Places API (New) Text Search in a small box around the OSM
//      position (see SEARCHES), field mask places.id only (that SKU is free).
//   3. Google Maps in the server's Chromium (connected over CDP): the "shortlist" page takes up to
//      20 place ids; "Select all" -> "Save" -> new list (first batch) or the list (later batches).
//   4. Share the list (public link) and store the backup lists/<slug>.json.
//
// Google has no API for saved lists, so step 3 drives the website. That is against Google's terms;
// if the account is blocked, `node server/atm-server.js restore` recreates all lists from the
// backups with another account and the site picks up the new links.

'use strict';

const fs = require('fs');
const path = require('path');
const { execFile } = require('child_process');

const STATE = process.env.ATM_STATE_DIR || path.join(process.env.HOME, '.local/share/atm-finder');
const LISTS_DIR = path.join(STATE, 'lists');
const CLI = process.env.ATM_FINDER_CLI || path.join(__dirname, '../target/release/atm-finder');
const CDP_URL = process.env.ATM_CDP_URL || 'http://127.0.0.1:9222';
const SHORTLIST_MAX = 20;
// Half the side of the search box around an OSM position, in degrees (~55 m north-south).
const BOX = 0.0005;

const log = (...args) => console.log(new Date().toISOString(), ...args);

// ---------------------------------------------------------------- backups

/** Every stored list, newest format: see saveList. */
function readLists() {
  if (!fs.existsSync(LISTS_DIR)) return [];
  return fs
    .readdirSync(LISTS_DIR)
    .filter((file) => file.endsWith('.json'))
    .map((file) => JSON.parse(fs.readFileSync(path.join(LISTS_DIR, file), 'utf8')))
    .sort((a, b) => a.name.localeCompare(b.name, 'de'));
}

/** Write lists/<slug>.json atomically. */
function saveList(list) {
  fs.mkdirSync(LISTS_DIR, { recursive: true });
  const file = path.join(LISTS_DIR, `${list.slug}.json`);
  fs.writeFileSync(`${file}.part`, JSON.stringify(list, null, 1));
  fs.renameSync(`${file}.part`, file);
  // Second copy on the backup disk right away (the daily timer copies everything again).
  const mirror = process.env.ATM_BACKUP_DIR || '/mnt/backup/atm-finder/lists';
  try {
    fs.mkdirSync(mirror, { recursive: true });
    fs.copyFileSync(file, path.join(mirror, `${list.slug}.json`));
  } catch (error) {
    log(`Sicherungskopie fehlgeschlagen: ${error.message}`);
  }
}

/** URL-safe file name like the CLI's slugify: lower case, umlauts spelled out, dashes. */
function slugify(name) {
  return name
    .toLowerCase()
    .replace(/ä/g, 'ae').replace(/ö/g, 'oe').replace(/ü/g, 'ue').replace(/ß/g, 'ss')
    .normalize('NFD').replace(/[̀-ͯ]/g, '')
    .replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
}

// ---------------------------------------------------------------- ATMs of a place

/** Parse an atm-finder GeoJSON export: the boundary feature and the ATM points. */
function parseExport(geojson) {
  const features = JSON.parse(geojson).features;
  const boundary = features.find((f) => f.geometry && f.geometry.type !== 'Point');
  const atms = features
    .filter((f) => f.geometry && f.geometry.type === 'Point')
    .map((f) => {
      const p = f.properties;
      const label = p.operator || p.brand || p.name || 'Bankomat';
      const address = [p.street, [p.postcode, p.city].filter(Boolean).join(' ')].filter(Boolean).join(', ');
      return { osm_id: p.id, lat: p.lat, lon: p.lon, label, address };
    });
  return { boundary: boundary ? boundary.properties : null, atms };
}

/** Export a place from OpenStreetMap with the CLI (a few Overpass requests). */
function exportRelation(relation) {
  return new Promise((resolve, reject) => {
    execFile(CLI, ['export', `r${relation}`, '-o', '-', '--format', 'geojson'], { maxBuffer: 64 << 20, timeout: 600000 }, (error, stdout, stderr) => {
      if (error) return reject(new Error(`atm-finder export: ${(stderr || error.message).trim().split('\n').pop()}`));
      resolve(parseExport(stdout));
    });
  });
}

// ---------------------------------------------------------------- Places API

function placesKey() {
  const file = path.join(STATE, 'places-key');
  if (!fs.existsSync(file)) throw new Error('Places-API-Schlüssel fehlt (places-key)');
  return fs.readFileSync(file, 'utf8').trim();
}

// Searches tried one after the other until one finds a place: an ATM place close by, an ATM
// place a little further away, then any place called "Geldautomat" close by (often the bank
// branch the machine belongs to). Measured for Graz: about 30 % / +10 % / +5 % of the ATMs.
const SEARCHES = [
  { textQuery: 'Bankomat', includedType: 'atm', box: BOX },
  { textQuery: 'Bankomat', includedType: 'atm', box: BOX * 2 },
  { textQuery: 'Geldautomat', box: BOX },
];

/** The Google place id of the ATM at an OSM position, or '' when Google has none there. */
async function placeIdNear(key, atm) {
  for (const search of SEARCHES) {
    const id = await searchText(key, atm, search);
    if (id) return id;
  }
  return '';
}

async function searchText(key, atm, { textQuery, includedType, box }) {
  const body = {
    textQuery,
    languageCode: 'de',
    regionCode: 'AT',
    pageSize: 1,
    locationRestriction: {
      rectangle: {
        low: { latitude: atm.lat - box, longitude: atm.lon - box * 1.5 },
        high: { latitude: atm.lat + box, longitude: atm.lon + box * 1.5 },
      },
    },
  };
  if (includedType) Object.assign(body, { includedType, strictTypeFiltering: true });
  for (let attempt = 1; ; attempt++) {
    const response = await fetch('https://places.googleapis.com/v1/places:searchText', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'X-Goog-Api-Key': key, 'X-Goog-FieldMask': 'places.id' },
      body: JSON.stringify(body),
    });
    if (response.ok) {
      const data = await response.json();
      return (data.places && data.places[0] && data.places[0].id) || '';
    }
    if (attempt >= 4 || (response.status !== 429 && response.status < 500)) {
      throw new Error(`Places API HTTP ${response.status}: ${(await response.text()).slice(0, 200)}`);
    }
    await new Promise((r) => setTimeout(r, 2000 * attempt));
  }
}

/** Add `place_id` to every ATM (keeps ids already known, e.g. from a backup). */
async function matchPlaces(atms, progress) {
  const key = placesKey();
  let done = 0;
  for (const atm of atms) {
    if (atm.place_id === undefined) atm.place_id = await placeIdNear(key, atm);
    progress && progress(++done, atms.length);
  }
}

// ---------------------------------------------------------------- Google Maps (browser)

let chromium;
function playwright() {
  chromium = chromium || require('playwright-core').chromium;
  return chromium;
}

/** Run `work(page)` in a new tab of the server's (logged-in) Chromium. */
async function withMapsPage(work) {
  const browser = await playwright().connectOverCDP(CDP_URL);
  const context = browser.contexts()[0];
  const page = await context.newPage();
  await page.setViewportSize({ width: 1400, height: 1000 });
  try {
    return await work(page);
  } catch (error) {
    // What the page looked like when it failed, for the next fix.
    await page.screenshot({ path: path.join(STATE, 'maps-error.png') }).catch(() => {});
    throw error;
  } finally {
    await page.close().catch(() => {});
    await browser.close().catch(() => {}); // disconnects only; the browser keeps running
  }
}

/** Save up to 20 place ids into the list `name` (created by the first batch). */
async function saveBatch(page, placeIds, name, create, retry = 0) {
  if (retry > 4) throw new Error(`Liste ${name} nicht im Speichern-Menü`);
  const url = `https://www.google.com/maps/@?api=1&map_action=shortlist&hl=en&place_ids=${placeIds.map(encodeURIComponent).join('%2C')}`;
  await page.goto(url, { waitUntil: 'domcontentloaded' });
  await page.getByRole('button', { name: 'Select all', exact: true }).click({ timeout: 60000 });
  await page.getByRole('button', { name: 'Save', exact: true }).click({ timeout: 30000 });
  await page.getByText('Save in your lists', { exact: true }).waitFor({ timeout: 30000 });
  if (create) {
    await page.getByRole('button', { name: 'New list' }).first().click();
    const dialog = page.getByRole('dialog', { name: 'New list' });
    await dialog.locator('input').first().fill(name);
    await dialog.getByRole('button', { name: 'Create', exact: true }).click();
  } else {
    // Lists in the menu: "<name> Private · n places", newest first. A list made seconds ago
    // may not be in the menu yet: reopen the page and try again.
    const entry = page.getByRole('menuitemradio', { name: listEntry(name) }).first();
    if (!(await entry.isVisible({ timeout: 10000 }).catch(() => false))) {
      await page.waitForTimeout(15000);
      return saveBatch(page, placeIds, name, false, (retry || 0) + 1);
    }
    // Checked = all selected places are in the list already (a retry); clicking would remove them.
    if ((await entry.getAttribute('aria-checked')) === 'true') return;
    await entry.click();
  }
  // A list made seconds ago sometimes does not take places yet (the Save button keeps spinning):
  // wait, then do the batch again. The count is checked at the end anyway.
  const saved = await page.getByText(`Saved to ${name}`).first().waitFor({ timeout: 60000 }).then(() => true, () => false);
  if (!saved) {
    log(`${name}: keine Bestätigung, neuer Versuch`);
    await page.waitForTimeout(20000);
    return saveBatch(page, placeIds, name, false, retry + 1);
  }
}

/** How many places the list `name` has, from its row in the Saved panel. */
async function placeCount(page, name) {
  await page.goto('https://www.google.com/maps/@47.07,15.44,12z/data=!4m2!10m1!1e1?hl=en', { waitUntil: 'domcontentloaded' });
  const row = page.getByRole('button', { name: listEntry(name) }).first();
  await row.waitFor({ timeout: 60000 });
  const match = (await row.innerText()).match(/(\d+) places?/);
  return match ? Number(match[1]) : 0;
}

/** Open the "More options" menu of a list in the Saved panel; false when there is no such list. */
async function openListMenu(page, name) {
  await page.goto('https://www.google.com/maps/@47.07,15.44,12z/data=!4m2!10m1!1e1?hl=en', { waitUntil: 'domcontentloaded' });
  await page.getByRole('button', { name: 'New list' }).first().waitFor({ timeout: 60000 });
  const row = page.getByRole('button', { name: listEntry(name) }).first();
  if (!(await row.isVisible({ timeout: 5000 }).catch(() => false))) return false;
  await row.locator('xpath=..').getByRole('button', { name: 'More options' }).click();
  return true;
}

/** Delete every list called `name` (left over from a failed run). */
async function deleteLists(page, name) {
  while (await openListMenu(page, name)) {
    await page.getByRole('menuitemradio', { name: 'Delete list' }).click();
    await page.locator('[role=dialog]').filter({ hasText: 'Are you sure' }).getByRole('button', { name: 'Delete', exact: true }).click();
    await page.waitForTimeout(3000);
    log(`alte Liste ${name} gelöscht`);
  }
}

/** Make the list viewable by link ("Send link to view") and return that link. */
async function shareList(page, name) {
  if (!(await openListMenu(page, name))) throw new Error(`Liste ${name} nicht gefunden`);
  await page.getByRole('menuitemradio', { name: 'Send link to view' }).click();
  const dialog = page.getByRole('dialog', { name: 'Send link to view' });
  const linkField = dialog.locator('input[value^="https://maps.app.goo.gl/"]');
  // A private list asks first: "Get link to share", then "Get link" to confirm.
  if (!(await linkField.first().isVisible({ timeout: 3000 }).catch(() => false))) {
    await dialog.getByRole('button', { name: /get link to share/i }).click();
    await dialog.getByRole('button', { name: /^get link$/i }).click();
  }
  await linkField.first().waitFor({ timeout: 30000 });
  return linkField.first().inputValue();
}

/** A list in Google's menus: "<name> Private · 3 places" (exact name, so Graz is not Graz-Umgebung). */
function listEntry(name) {
  return new RegExp(`^\\s*${escapeRegExp(name)} (Private|Shared|Public)`);
}

function escapeRegExp(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/** Put the matched ATMs of `list` into a new public Google Maps list; returns the share link. */
async function buildGoogleList(list, progress) {
  const ids = [...new Set(list.atms.map((atm) => atm.place_id).filter(Boolean))];
  if (ids.length === 0) throw new Error('Google kennt keinen dieser Bankomaten als Ort');
  return withMapsPage(async (page) => {
    await deleteLists(page, list.list_name);
    for (let start = 0; start < ids.length; start += SHORTLIST_MAX) {
      await saveBatch(page, ids.slice(start, start + SHORTLIST_MAX), list.list_name, start === 0);
      if (start === 0) await page.waitForTimeout(20000); // let Google settle the new list
      progress && progress(Math.min(start + SHORTLIST_MAX, ids.length), ids.length);
    }
    // The count in the Saved panel lags behind; if it stays short, save all batches once more
    // (batches that are complete already are skipped, see saveBatch).
    let count = 0;
    for (let round = 0; round < 6 && count < ids.length; round++) {
      if (round > 0) await page.waitForTimeout(20000);
      if (round === 3) {
        log(`${list.list_name}: ${count}/${ids.length}, Gruppen noch einmal`);
        for (let start = 0; start < ids.length; start += SHORTLIST_MAX) {
          await saveBatch(page, ids.slice(start, start + SHORTLIST_MAX), list.list_name, false);
        }
      }
      count = await placeCount(page, list.list_name);
    }
    if (count < ids.length) throw new Error(`${list.list_name}: nur ${count} von ${ids.length} Orten gespeichert`);
    return shareList(page, list.list_name);
  });
}

// ---------------------------------------------------------------- one place

/** The list name in Google Maps (max. 40 characters). */
function listName(name) {
  return `Bankomaten ${name}`.slice(0, 40);
}

/**
 * Create the list for a place end to end and store the backup.
 * `source`: { relation } (export from OSM now) or { geojsonFile, slug } (ready-made export),
 * or { backup } (restore: reuse ATMs and place ids from an old backup).
 */
async function createList(source, progress = () => {}) {
  placesKey(); // fail before the export when the key is missing
  let place;
  let slug = source.slug;
  if (source.backup) {
    place = { boundary: { relation: source.backup.relation, name: source.backup.name }, atms: source.backup.atms };
  } else if (source.geojsonFile) {
    place = parseExport(fs.readFileSync(source.geojsonFile, 'utf8'));
  } else {
    progress('export');
    place = await exportRelation(source.relation);
  }
  if (!place.boundary) throw new Error('Gebiet nicht gefunden');
  if (place.atms.length === 0) throw new Error('keine Bankomaten in diesem Gebiet');
  const name = (source.backup && source.backup.name) || place.boundary.name;
  const relation = place.boundary.relation;
  if (!slug) {
    slug = slugify(name);
    const taken = readLists().find((other) => other.slug === slug && other.relation !== relation);
    if (taken) slug = `${slug}-${relation}`;
  }
  const list = {
    slug,
    name,
    relation,
    list_name: listName(name),
    url: '',
    created: '',
    account: process.env.ATM_GOOGLE_ACCOUNT || '',
    atms: place.atms,
  };
  progress('places');
  await matchPlaces(list.atms, (done, total) => progress('places', done, total));
  log(`${name}: ${list.atms.filter((a) => a.place_id).length}/${list.atms.length} mit Google-Ort`);
  // Keep the matches even if Google Maps fails below; the next run reuses them (url still '').
  saveList(list);
  progress('maps');
  list.url = await buildGoogleList(list, (done, total) => progress('maps', done, total));
  list.created = new Date().toISOString();
  list.in_list = list.atms.filter((atm) => atm.place_id).length;
  list.total = list.atms.length;
  saveList(list);
  log(`${name}: Liste ${list.url}`);
  return list;
}

module.exports = { STATE, LISTS_DIR, readLists, saveList, createList, matchPlaces, slugify, parseExport, log };
