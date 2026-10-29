// Per-visitor limits for creating Google Maps lists, on top of the global lock: at most PER_HOUR
// lists per hour and PER_DAY per day from one address; after BLOCK_AFTER lists in total that address
// can create none at all. Addresses are stored only as a salted hash (quota.json next to lock.json).

'use strict';

const crypto = require('crypto');
const fs = require('fs');
const path = require('path');

const HOUR = 3600000;
const DAY = 24 * HOUR;

function limits(env = process.env) {
  return {
    perHour: Number(env.ATM_IP_PER_HOUR || 1),
    perDay: Number(env.ATM_IP_PER_DAY || 3),
    blockAfter: Number(env.ATM_IP_BLOCK_AFTER || 20),
  };
}

function load(file) {
  let data;
  try {
    data = JSON.parse(fs.readFileSync(file, 'utf8'));
  } catch {
    data = {};
  }
  if (typeof data.salt !== 'string') data.salt = crypto.randomBytes(16).toString('hex');
  if (!data.visitors || typeof data.visitors !== 'object') data.visitors = {};
  return data;
}

function save(file, data) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(`${file}.part`, JSON.stringify(data));
  fs.renameSync(`${file}.part`, file);
}

const key = (data, address) => crypto.createHash('sha256').update(`${data.salt}:${address}`).digest('hex').slice(0, 24);

/**
 * Why this visitor may not start a list now, or null. `until` is the time (ms) from which the
 * next list is possible again (0 when blocked for good); `limit` is the number that was reached.
 */
function check(data, address, now, lim) {
  const visitor = data.visitors[key(data, address)];
  if (!visitor) return null;
  if (visitor.total >= lim.blockAfter) return { reason: 'blocked', until: 0, limit: lim.blockAfter };
  const lastHour = visitor.times.filter((t) => now - t < HOUR);
  if (lastHour.length >= lim.perHour) return { reason: 'hour', until: Math.min(...lastHour) + HOUR, limit: lim.perHour };
  const lastDay = visitor.times.filter((t) => now - t < DAY);
  if (lastDay.length >= lim.perDay) return { reason: 'day', until: Math.min(...lastDay) + DAY, limit: lim.perDay };
  return null;
}

/** Count a started list for this visitor; times older than a day are dropped. */
function record(data, address, now) {
  const k = key(data, address);
  const visitor = data.visitors[k] || { times: [], total: 0 };
  visitor.times = visitor.times.filter((t) => now - t < DAY).concat(now);
  visitor.total += 1;
  data.visitors[k] = visitor;
}

module.exports = { limits, load, save, check, record, HOUR, DAY };
