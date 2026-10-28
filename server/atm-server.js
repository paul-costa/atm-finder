#!/usr/bin/env node
// atm-finder server on costaserver: serves the website (web/dist) and the Google Maps list API.
//
//   node server/atm-server.js serve              website + API on 127.0.0.1:8090 (ATM_PORT)
//   node server/atm-server.js create-ready       create the missing lists for data/*.geojson
//   node server/atm-server.js restore            recreate every list from its backup (new account)
//
// API (same origin as the site):
//   GET  /api/lists   -> { lists: [{slug, name, relation, url, in_list, total}], lock }
//   POST /api/lists   {relation, name} -> 202 started | 409 locked | 200 exists (same body)
//
// Lock: one list at a time for everybody; after each run (success or not) a cooldown of
// 30 minutes (ATM_COOLDOWN_MIN). The lock lives in lock.json so restarts keep it.

'use strict';

const fs = require('fs');
const http = require('http');
const path = require('path');
const lists = require('./lists');

const PORT = Number(process.env.ATM_PORT || 8090);
const DIST = process.env.ATM_DIST || path.join(__dirname, '../web/dist');
const DATA = process.env.ATM_DATA || path.join(__dirname, '../data');
const HOST = 'atm-finder.costapaul.com';
const OLD_HOST = 'atm.costapaul.com';
const COOLDOWN_MS = Number(process.env.ATM_COOLDOWN_MIN || 30) * 60000;
const LOCK_FILE = path.join(lists.STATE, 'lock.json');
const { log } = lists;

// ---------------------------------------------------------------- lock

function readLock() {
  try {
    return JSON.parse(fs.readFileSync(LOCK_FILE, 'utf8'));
  } catch {
    return { state: 'free' };
  }
}

function writeLock(lock) {
  fs.mkdirSync(lists.STATE, { recursive: true });
  fs.writeFileSync(`${LOCK_FILE}.part`, JSON.stringify(lock));
  fs.renameSync(`${LOCK_FILE}.part`, LOCK_FILE);
}

const hhmm = (ms) => new Date(ms).toLocaleTimeString('de-AT', { hour: '2-digit', minute: '2-digit', timeZone: 'Europe/Vienna' });

/** The lock as the website sees it; a running lock of a dead process counts as finished. */
function publicLock() {
  const lock = readLock();
  if (lock.state === 'running' && lock.pid && !alive(lock.pid)) {
    lock.state = 'done';
    lock.finished = lock.started;
    lock.error = lock.error || 'abgebrochen';
  }
  if (lock.state === 'running') {
    return { state: 'running', place: lock.place, relation: lock.relation, until: '', error: '' };
  }
  const until = (lock.finished || 0) + COOLDOWN_MS;
  const error = lock.error || '';
  if (lock.finished && Date.now() < until) {
    return { state: 'cooldown', place: lock.place, relation: lock.relation, until: hhmm(until), error };
  }
  return { state: 'free', place: '', relation: lock.relation || 0, until: '', error };
}

function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function state() {
  const entries = lists.readLists().filter((list) => list.url).map(({ slug, name, relation, url, in_list, total }) => ({ slug, name, relation, url, in_list, total }));
  return { lists: entries, lock: publicLock() };
}

/** Take the lock and run `work`; returns false when it is held or cooling down. */
function runLocked(place, relation, work, { ignoreCooldown = false } = {}) {
  const current = publicLock();
  if (current.state === 'running' || (current.state === 'cooldown' && !ignoreCooldown)) return false;
  writeLock({ state: 'running', place, relation, started: Date.now(), pid: process.pid });
  const finish = (error) => {
    writeLock({ state: 'done', place, relation, started: readLock().started, finished: Date.now(), error: error ? String(error.message || error).slice(0, 200) : '' });
  };
  Promise.resolve()
    .then(work)
    .then(() => finish(null), (error) => {
      log(`Fehler (${place}):`, error.stack || error);
      finish(error);
    });
  return true;
}

// ---------------------------------------------------------------- HTTP

const TYPES = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8',
  '.wasm': 'application/wasm', '.json': 'application/json', '.svg': 'image/svg+xml', '.png': 'image/png',
  '.csv': 'text/csv; charset=utf-8', '.geojson': 'application/geo+json', '.kml': 'application/vnd.google-earth.kml+xml',
  '.md': 'text/plain; charset=utf-8', '.txt': 'text/plain; charset=utf-8',
};

function send(res, status, body, type = 'application/json', headers = {}) {
  res.writeHead(status, { 'Content-Type': type, 'X-Content-Type-Options': 'nosniff', ...headers });
  res.end(body);
}

function serveStatic(req, res, pathname) {
  let rel = decodeURIComponent(pathname);
  if (rel.endsWith('/')) rel += 'index.html';
  const file = path.normalize(path.join(DIST, rel));
  if (!file.startsWith(DIST + path.sep)) return send(res, 403, 'forbidden', 'text/plain');
  fs.stat(file, (error, stat) => {
    if (error || !stat.isFile()) return send(res, 404, 'not found', 'text/plain');
    const ext = path.extname(file);
    // Hashless file names: let browsers revalidate, the files are small.
    res.writeHead(200, { 'Content-Type': TYPES[ext] || 'application/octet-stream', 'Content-Length': stat.size, 'Cache-Control': 'no-cache', 'X-Content-Type-Options': 'nosniff' });
    if (req.method === 'HEAD') return res.end();
    fs.createReadStream(file).pipe(res);
  });
}

function readBody(req, limit = 4096) {
  return new Promise((resolve, reject) => {
    let body = '';
    req.on('data', (chunk) => {
      body += chunk;
      if (body.length > limit) {
        reject(new Error('too large'));
        req.destroy();
      }
    });
    req.on('end', () => resolve(body));
    req.on('error', reject);
  });
}

async function onCreate(req, res) {
  let request;
  try {
    request = JSON.parse(await readBody(req));
  } catch {
    return send(res, 400, JSON.stringify({ error: 'bad request' }));
  }
  const relation = Number(request.relation);
  const name = String(request.name || '').slice(0, 80);
  if (!Number.isSafeInteger(relation) || relation <= 0) return send(res, 400, JSON.stringify({ error: 'relation' }));
  if (lists.readLists().some((list) => list.relation === relation && list.url)) return send(res, 200, JSON.stringify(state()));
  const started = runLocked(name || `r${relation}`, relation, () => lists.createList({ relation }));
  if (started) log(`Liste angefordert: ${name} (r${relation})`);
  send(res, started ? 202 : 409, JSON.stringify(state()));
}

function serve() {
  const server = http.createServer((req, res) => {
    const { pathname } = new URL(req.url, 'http://localhost');
    // The old name atm.costapaul.com points here too; keep its links working.
    if ((req.headers.host || '').split(':')[0] === OLD_HOST) {
      return send(res, 301, '', 'text/plain', { Location: `https://${HOST}${req.url}` });
    }
    if (pathname === '/api/lists') {
      if (req.method === 'GET') return send(res, 200, JSON.stringify(state()), 'application/json', { 'Cache-Control': 'no-store' });
      if (req.method === 'POST') return onCreate(req, res).catch(() => send(res, 500, '{}'));
      return send(res, 405, '{}');
    }
    if (req.method !== 'GET' && req.method !== 'HEAD') return send(res, 405, 'method not allowed', 'text/plain');
    serveStatic(req, res, pathname);
  });
  server.listen(PORT, '127.0.0.1', () => log(`atm-server auf http://127.0.0.1:${PORT} (${DIST})`));
}

// ---------------------------------------------------------------- batch commands

/** Run `jobs` one after the other under the lock (ignoring the cooldown); wait for the end. */
function runJobs(label, jobs) {
  return new Promise((resolve) => {
    const ok = runLocked(label, 0, async () => {
      for (const job of jobs) {
        try {
          await job();
        } catch (error) {
          log('Fehler:', error.message);
        }
      }
      resolve();
    }, { ignoreCooldown: true });
    if (!ok) {
      log('Gerade läuft eine Erstellung, später noch einmal versuchen.');
      process.exitCode = 1;
      resolve();
    }
  });
}

/** Lists for the ready-made exports that do not have one yet. */
async function createReady() {
  const index = JSON.parse(fs.readFileSync(path.join(DIST, 'data/index.json'), 'utf8'));
  const stored = new Map(lists.readLists().map((list) => [list.slug, list]));
  const jobs = index
    .filter((entry) => !(stored.get(entry.slug) || {}).url)
    .map((entry) => () => {
      // A run that failed in Google Maps left its matches behind: reuse them.
      const partial = stored.get(entry.slug);
      if (partial) return lists.createList({ slug: entry.slug, backup: partial });
      return lists.createList({ slug: entry.slug, geojsonFile: path.join(DATA, `${entry.slug}.geojson`) });
    });
  log(`${jobs.length} fehlende Listen`);
  await runJobs('fertige Orte', jobs);
}

/** Recreate every list from its backup in the account Chromium is logged in with now. */
async function restore() {
  const backups = lists.readLists();
  log(`${backups.length} Listen werden neu angelegt`);
  const jobs = backups.map((backup) => async () => {
    const archive = path.join(lists.STATE, 'lists-old', new Date().toISOString().slice(0, 10));
    fs.mkdirSync(archive, { recursive: true });
    fs.copyFileSync(path.join(lists.LISTS_DIR, `${backup.slug}.json`), path.join(archive, `${backup.slug}.json`));
    await lists.createList({ slug: backup.slug, backup });
  });
  await runJobs('Wiederherstellung', jobs);
}

const command = process.argv[2] || 'serve';
if (command === 'serve') serve();
else if (command === 'create-ready') createReady();
else if (command === 'restore') restore();
else {
  console.error('Befehl: serve | create-ready | restore');
  process.exit(2);
}
