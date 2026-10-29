// node --test server/quota.test.js  (run by deploy.sh before anything goes live)
'use strict';

const assert = require('assert');
const fs = require('fs');
const os = require('os');
const path = require('path');
const test = require('node:test');
const quota = require('./quota');

const lim = { perHour: 1, perDay: 3, blockAfter: 20 };
const T0 = Date.UTC(2026, 9, 29, 8, 0);
const fresh = () => ({ salt: 'test', visitors: {} });

test('a new visitor may create a list', () => {
  assert.strictEqual(quota.check(fresh(), '1.2.3.4', T0, lim), null);
});

test('one list per hour, measured from the last one', () => {
  const data = fresh();
  quota.record(data, '1.2.3.4', T0);
  const denied = quota.check(data, '1.2.3.4', T0 + 30 * 60000, lim);
  assert.deepStrictEqual(denied, { reason: 'hour', until: T0 + quota.HOUR, limit: 1 });
  assert.strictEqual(quota.check(data, '1.2.3.4', T0 + quota.HOUR, lim), null);
  assert.strictEqual(quota.check(data, '5.6.7.8', T0 + 60000, lim), null, 'other visitors are not affected');
});

test('three lists per day', () => {
  const data = fresh();
  for (let i = 0; i < 3; i++) quota.record(data, '1.2.3.4', T0 + i * 2 * quota.HOUR);
  const denied = quota.check(data, '1.2.3.4', T0 + 6 * quota.HOUR, lim);
  assert.deepStrictEqual(denied, { reason: 'day', until: T0 + quota.DAY, limit: 3 });
  assert.strictEqual(quota.check(data, '1.2.3.4', T0 + quota.DAY, lim), null);
});

test('blocked for good after 20 lists, also days later', () => {
  const data = fresh();
  for (let i = 0; i < 20; i++) quota.record(data, '1.2.3.4', T0 + i * quota.DAY);
  const later = T0 + 400 * quota.DAY;
  assert.deepStrictEqual(quota.check(data, '1.2.3.4', later, lim), { reason: 'blocked', until: 0, limit: 20 });
  assert.ok(data.visitors[Object.keys(data.visitors)[0]].times.length <= 3, 'old times are dropped');
});

test('addresses are stored only as a hash', () => {
  const file = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'quota-')), 'quota.json');
  const data = quota.load(file);
  quota.record(data, '203.0.113.7', T0);
  quota.save(file, data);
  const text = fs.readFileSync(file, 'utf8');
  assert.ok(!text.includes('203.0.113.7'));
  assert.strictEqual(quota.check(quota.load(file), '203.0.113.7', T0 + 1, lim).reason, 'hour', 'the salt survives a reload');
});

test('limits come from the environment', () => {
  assert.deepStrictEqual(quota.limits({ ATM_IP_PER_HOUR: '2', ATM_IP_PER_DAY: '5', ATM_IP_BLOCK_AFTER: '30' }),
    { perHour: 2, perDay: 5, blockAfter: 30 });
  assert.deepStrictEqual(quota.limits({}), { perHour: 1, perDay: 3, blockAfter: 20 });
});
