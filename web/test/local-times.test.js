import assert from 'node:assert/strict';
import { test } from 'node:test';

import { localizeTime, localizeTimes, rememberZone } from '../src/local-times.js';
import { ZONE_COOKIE } from '../src/zone-cookie.js';

/**
 * A `<time>` element as far as the module reads it.
 * @param {string} datetime
 * @param {string} text
 */
function timeElement(datetime, text) {
  return {
    textContent: text,
    /** @param {string} name */
    getAttribute: (name) => (name === 'datetime' ? datetime : null),
  };
}

/**
 * A scope holding `times`, answering only the selector the module asks with.
 * @param {ReturnType<typeof timeElement>[]} times
 * @returns {ParentNode}
 */
function scopeOf(times) {
  return /** @type {ParentNode} */ (
    /** @type {unknown} */ ({
      /** @param {string} selector */
      querySelectorAll: (selector) => (selector === 'time[data-clock]' ? times : []),
    })
  );
}

/** @param {Date} at */
const clockAt = (at) => ({ now: () => at });

test('a time of the clock\'s day shows hours and minutes, one of another day its date too', () => {
  const now = new Date(2026, 9, 19, 14, 32);
  const today = timeElement(new Date(2026, 9, 19, 9, 5).toISOString(), 'server text');
  const earlier = timeElement(new Date(2026, 9, 18, 23, 59).toISOString(), 'server text');

  localizeTimes(scopeOf([today, earlier]), clockAt(now));

  assert.equal(today.textContent, '09:05');
  assert.equal(earlier.textContent, '2026-10-18 23:59');
});

test('the same time is today against one clock and another day against a later one', () => {
  const at = new Date(2026, 9, 19, 9, 5).toISOString();
  const first = timeElement(at, '');
  const second = timeElement(at, '');

  localizeTimes(scopeOf([first]), clockAt(new Date(2026, 9, 19, 23, 0)));
  localizeTimes(scopeOf([second]), clockAt(new Date(2026, 9, 20, 0, 5)));

  assert.equal(first.textContent, '09:05');
  assert.equal(second.textContent, '2026-10-19 09:05');
});

test('a time whose datetime is not a time keeps the text the server wrote', () => {
  const broken = timeElement('soon', 'server text');

  localizeTimes(scopeOf([broken]), clockAt(new Date(2026, 9, 19, 14, 32)));
  localizeTime(/** @type {Element} */ (/** @type {unknown} */ (broken)), new Date(2026, 9, 19, 14, 32));

  assert.equal(broken.textContent, 'server text');
});

test("the zone recorded is the offset at the clock's time", () => {
  const now = new Date(2026, 9, 19, 14, 32);
  let written = '';
  const doc = {
    get cookie() {
      return '';
    },
    set cookie(value) {
      written = value;
    },
  };

  rememberZone(doc, clockAt(now));

  assert.ok(written.startsWith(`${ZONE_COOKIE}=${-now.getTimezoneOffset() || 0};`), written);
});

test('a zone already recorded is not written again', () => {
  const now = new Date(2026, 9, 19, 14, 32);
  /** @type {string[]} */
  const written = [];
  const doc = {
    get cookie() {
      return `${ZONE_COOKIE}=${-now.getTimezoneOffset() || 0}`;
    },
    set cookie(value) {
      written.push(value);
    },
  };

  rememberZone(doc, clockAt(now));

  assert.deepEqual(written, []);
});
