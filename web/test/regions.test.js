import assert from 'node:assert/strict';
import { test } from 'node:test';

import { answerVerdict, carriedState, requestRegions } from '../src/regions.js';

/**
 * A node of a parsed page, as far as the regions are read from it.
 * @typedef {{ name: string, getAttribute(attribute: string): string | null }} FakeRegion
 */

/**
 * @param {string} name
 * @returns {FakeRegion}
 */
function region(name) {
  return { name, getAttribute: (attribute) => (attribute === 'data-region' ? name : null) };
}

/**
 * A parser that answers a page holding these regions, whatever the text.
 * @param {readonly string[]} names
 * @returns {(html: string) => { querySelectorAll(selectors: string): FakeRegion[] }}
 */
function pageWith(names) {
  return () => ({ querySelectorAll: () => names.map(region) });
}

/**
 * @param {number} status
 * @param {{ contentType?: string, redirectedTo?: string }} [options]
 * @returns {Response}
 */
function answer(status, { contentType = 'text/html; charset=utf-8', redirectedTo } = {}) {
  const response = new Response('<!doctype html>', { status, headers: { 'Content-Type': contentType } });
  if (redirectedTo !== undefined) {
    Object.defineProperty(response, 'redirected', { value: true });
    Object.defineProperty(response, 'url', { value: redirectedTo });
  }
  return response;
}

/**
 * A fetch that answers `response` and records what it was asked.
 * @param {Response} response
 */
function fetchAnswering(response) {
  /** @type {{ url: string, init: RequestInit | undefined }[]} */
  const calls = [];
  /** @type {typeof fetch} */
  const fetchImpl = async (input, init) => {
    calls.push({ url: String(input), init });
    return response;
  };
  return { fetchImpl, calls };
}

const PAGE = 'http://dibs.example.com/ui/claims/7?who=alice';
const WANTED = ['rail', 'list', 'inspector'];
const LOGIN = { loginPath: '/ui/login' };

test('a page of the service is asked for as HTML over the session, with a deadline', async () => {
  const { fetchImpl, calls } = fetchAnswering(answer(200));
  await requestRegions(PAGE, WANTED, pageWith(WANTED), { fetchImpl, ...LOGIN });
  assert.equal(calls.length, 1);
  const [call] = calls;
  assert.equal(call?.url, PAGE);
  assert.equal(call?.init?.method, 'GET');
  assert.equal(call?.init?.credentials, 'same-origin');
  assert.equal(call?.init?.redirect, 'follow');
  assert.deepEqual(call?.init?.headers, { Accept: 'text/html' });
  assert.ok(call?.init?.signal instanceof AbortSignal);
});

test('a 200 page holding every region asked for answers those regions by name', async () => {
  const { fetchImpl } = fetchAnswering(answer(200));
  const result = await requestRegions(PAGE, WANTED, pageWith(['rail', 'list', 'inspector', 'results']), {
    fetchImpl,
    ...LOGIN,
  });
  assert.equal(result.kind, 'ok');
  if (result.kind === 'ok') {
    assert.deepEqual([...result.regions.keys()].sort(), ['inspector', 'list', 'rail']);
    assert.equal(result.regions.get('list')?.name, 'list');
  }
});

test('a 404 page holding the regions is swapped in too: it says what is gone', async () => {
  const { fetchImpl } = fetchAnswering(answer(404));
  const result = await requestRegions(PAGE, ['inspector'], pageWith(WANTED), { fetchImpl, ...LOGIN });
  assert.equal(result.kind, 'ok');
});

test('a page missing one region asked for fails, so nothing is half swapped', async (t) => {
  t.mock.method(console, 'error', () => {});
  const { fetchImpl } = fetchAnswering(answer(200));
  const result = await requestRegions(PAGE, WANTED, pageWith(['rail', 'list']), { fetchImpl, ...LOGIN });
  assert.deepEqual(result, { kind: 'failed' });
});

test('a 401 means the session ended', async () => {
  const { fetchImpl } = fetchAnswering(answer(401, { contentType: 'application/json' }));
  const result = await requestRegions(PAGE, WANTED, pageWith(WANTED), { fetchImpl, ...LOGIN });
  assert.deepEqual(result, { kind: 'signed-out' });
});

test('a redirect that lands on the sign-in page means the session ended, whatever that page holds', async () => {
  const { fetchImpl } = fetchAnswering(answer(200, { redirectedTo: 'http://dibs.example.com/ui/login?next=x' }));
  const result = await requestRegions(PAGE, WANTED, pageWith(WANTED), { fetchImpl, ...LOGIN });
  assert.deepEqual(result, { kind: 'signed-out' });
});

test('a redirect to another page of the service is read like any page', async () => {
  const { fetchImpl } = fetchAnswering(answer(200, { redirectedTo: 'http://dibs.example.com/ui/claims' }));
  const result = await requestRegions(PAGE, WANTED, pageWith(WANTED), { fetchImpl, ...LOGIN });
  assert.equal(result.kind, 'ok');
});

test('a 500, a 403, and a 200 that is not HTML, fail', async (t) => {
  t.mock.method(console, 'error', () => {});
  for (const response of [answer(500), answer(403), answer(200, { contentType: 'application/json' })]) {
    const { fetchImpl } = fetchAnswering(response);
    const result = await requestRegions(PAGE, WANTED, pageWith(WANTED), { fetchImpl, ...LOGIN });
    assert.deepEqual(result, { kind: 'failed' }, String(response.status));
  }
});

test('a network error fails', async (t) => {
  t.mock.method(console, 'error', () => {});
  /** @type {typeof fetch} */
  const fetchImpl = async () => {
    throw new TypeError('network down');
  };
  const result = await requestRegions(PAGE, WANTED, pageWith(WANTED), { fetchImpl, ...LOGIN });
  assert.deepEqual(result, { kind: 'failed' });
});

/** A fetch that answers only when its signal aborts, with the signal's reason. @type {typeof fetch} */
const fetchHanging = (_input, init) =>
  new Promise((_resolve, reject) => {
    const signal = init?.signal;
    signal?.addEventListener('abort', () => reject(signal.reason), { once: true });
  });

test('a request that outlasts its deadline fails', async (t) => {
  t.mock.method(console, 'error', () => {});
  const result = await requestRegions(PAGE, WANTED, pageWith(WANTED), {
    fetchImpl: fetchHanging,
    timeoutMs: 5,
    ...LOGIN,
  });
  assert.deepEqual(result, { kind: 'failed' });
});

test('an abort by the caller rejects with the AbortError, so a newer navigation drops the older one', async () => {
  const controller = new AbortController();
  const pending = requestRegions(PAGE, WANTED, pageWith(WANTED), {
    fetchImpl: fetchHanging,
    signal: controller.signal,
    ...LOGIN,
  });
  controller.abort();
  await assert.rejects(pending, { name: 'AbortError' });
});

test('an abort that comes after the answer arrived still rejects: an aborted caller never gets regions', async () => {
  const controller = new AbortController();
  /** @type {typeof fetch} */
  const fetchImpl = async () => {
    controller.abort();
    return answer(200);
  };
  const pending = requestRegions(PAGE, WANTED, pageWith(WANTED), { fetchImpl, signal: controller.signal, ...LOGIN });
  await assert.rejects(pending, { name: 'AbortError' });
});

test('the verdict on an answer before it is read', () => {
  const html = 'text/html; charset=utf-8';
  const plain = { redirected: false, finalPath: '/ui/claims', contentType: html };
  assert.equal(answerVerdict({ ...plain, status: 200 }, '/ui/login'), 'read');
  assert.equal(answerVerdict({ ...plain, status: 404 }, '/ui/login'), 'read');
  assert.equal(answerVerdict({ ...plain, status: 200, contentType: 'TEXT/HTML' }, '/ui/login'), 'read');
  assert.equal(answerVerdict({ ...plain, status: 401, contentType: '' }, '/ui/login'), 'signed-out');
  assert.equal(
    answerVerdict({ ...plain, status: 200, redirected: true, finalPath: '/ui/login' }, '/ui/login'),
    'signed-out',
  );
  assert.equal(answerVerdict({ ...plain, status: 200, finalPath: '/ui/login' }, '/ui/login'), 'read');
  assert.equal(answerVerdict({ ...plain, status: 200, redirected: true, finalPath: '/ui/login' }, undefined), 'read');
  assert.equal(answerVerdict({ ...plain, status: 500 }, '/ui/login'), 'failed');
  assert.equal(answerVerdict({ ...plain, status: 302 }, '/ui/login'), 'failed');
  assert.equal(answerVerdict({ ...plain, status: 200, contentType: 'text/html-but-not' }, '/ui/login'), 'failed');
  assert.equal(answerVerdict({ ...plain, status: 200, contentType: '' }, '/ui/login'), 'failed');
});

test('a swap carries the rail state the scripts set and the open details, and nothing else', () => {
  assert.deepEqual(
    carriedState({
      attrs: { 'data-collapsible': 'true', 'data-open': 'true', 'data-region': 'rail', class: 'rail' },
      openDetails: ['slot-deploy', '', 'slot-deploy', 'slot-review'],
    }),
    { attrs: { 'data-collapsible': 'true', 'data-open': 'true' }, openDetails: ['slot-deploy', 'slot-review'] },
  );
});

test('an attribute the old region lacks is carried as absent, so the new region loses it too', () => {
  assert.deepEqual(carriedState({ attrs: { 'data-collapsible': 'true' }, openDetails: [] }), {
    attrs: { 'data-collapsible': 'true', 'data-open': null },
    openDetails: [],
  });
  assert.deepEqual(carriedState({ attrs: {}, openDetails: [] }), {
    attrs: { 'data-collapsible': null, 'data-open': null },
    openDetails: [],
  });
});
