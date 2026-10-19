import assert from 'node:assert/strict';
import { test } from 'node:test';

import { ZONE_COOKIE, cookieValue, zoneCookie } from '../src/zone-cookie.js';

test('a cookie is read by its exact name', () => {
  assert.equal(cookieValue('a=1; dibs_tz=120 ; b=2', 'dibs_tz'), '120');
  assert.equal(cookieValue('dibs_tz_old=5; x=dibs_tz=7', 'dibs_tz'), null);
  assert.equal(cookieValue('', 'dibs_tz'), null);
});

test('the offset is written with its flags when it is new or changed', () => {
  const written = zoneCookie('theme=dark', 120);
  assert.equal(written, `${ZONE_COOKIE}=120; Path=/ui; Max-Age=31536000; SameSite=Strict; Secure`);
  assert.equal(zoneCookie('dibs_tz=60', -300), `${ZONE_COOKIE}=-300; Path=/ui; Max-Age=31536000; SameSite=Strict; Secure`);
});

test('an offset already recorded is not written again', () => {
  assert.equal(zoneCookie('a=1; dibs_tz=120', 120), null);
  assert.equal(zoneCookie('dibs_tz=0', -0), null);
  assert.equal(zoneCookie('dibs_tz=330', 330.4), null);
});
