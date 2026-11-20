import assert from 'node:assert/strict';
import { test } from 'node:test';

import { closesOnKey, isPlainClick } from '../src/inspect-input.js';
import { closeLinkIndex, selectedIndex } from '../src/list-selection.js';
import { openingAction, regionsToAsk } from '../src/opening-regions.js';
import { pathOf, sameOrigin, samePath } from '../src/page-addresses.js';

const plain = {
  button: 0,
  ctrlKey: false,
  metaKey: false,
  shiftKey: false,
  altKey: false,
  defaultPrevented: false,
};

test('a click with the primary button and no modifier is a plain click', () => {
  assert.equal(isPlainClick(plain), true);
});

test('a click with a modifier key opens the link as the browser does', () => {
  for (const key of /** @type {const} */ (['ctrlKey', 'metaKey', 'shiftKey', 'altKey'])) {
    assert.equal(isPlainClick({ ...plain, [key]: true }), false, key);
  }
});

test('a click with the middle or another button is left to the browser', () => {
  assert.equal(isPlainClick({ ...plain, button: 1 }), false);
  assert.equal(isPlainClick({ ...plain, button: 2 }), false);
});

test('a click another script already handled is left alone', () => {
  assert.equal(isPlainClick({ ...plain, defaultPrevented: true }), false);
});

test('two addresses with one path are the same page, whatever their query or fragment', () => {
  const base = 'http://dibs.example.com/ui/claims/7';
  assert.equal(samePath(base, base), true);
  assert.equal(samePath(base, `${base}?who=alice`), true);
  assert.equal(samePath(`${base}?who=bob#notes`, `${base}?who=alice`), true);
  assert.equal(samePath(base, 'http://dibs.example.com/ui/claims/8'), false);
  assert.equal(samePath(base, 'http://dibs.example.com/ui/claims'), false);
});

test('an address on another origin is never the same page', () => {
  assert.equal(samePath('http://dibs.example.com/ui/claims/7', 'https://dibs.example.com/ui/claims/7'), false);
  assert.equal(samePath('http://dibs.example.com/ui/claims/7', 'http://other.example.com/ui/claims/7'), false);
  assert.equal(samePath('http://dibs.example.com/ui/claims/7', '/ui/claims/7'), false);
});

test('two addresses are on one origin when scheme, host and port agree', () => {
  assert.equal(sameOrigin('http://dibs.example.com/ui/claims', 'http://dibs.example.com/ui/inbox?tab=dibs'), true);
  assert.equal(sameOrigin('http://dibs.example.com/ui/claims', 'http://dibs.example.com:8080/ui/claims'), false);
  assert.equal(sameOrigin('http://dibs.example.com/ui/claims', 'not an address'), false);
});

test('only an absolute address has a path', () => {
  assert.equal(pathOf('http://dibs.example.com/ui/areas/widgets:search?x=1#y'), '/ui/areas/widgets:search');
  assert.equal(pathOf('/ui/claims'), null);
  assert.equal(pathOf(''), null);
});

test('the selected row is the one whose link points at the page shown', () => {
  const rows = [
    'http://dibs.example.com/ui/claims/7?who=alice',
    '',
    'http://dibs.example.com/ui/claims/12?who=alice',
  ];
  assert.equal(selectedIndex(rows, '/ui/claims/12'), 2);
  assert.equal(selectedIndex(rows, '/ui/claims/7'), 0);
  assert.equal(selectedIndex(rows, '/ui/claims'), -1);
  assert.equal(selectedIndex([], '/ui/claims/7'), -1);
});

test('a seat on Areas is selected by its own address, colon and all', () => {
  const seats = ['http://dibs.example.com/ui/areas/widgets:search', 'http://dibs.example.com/ui/areas/widgets:billing'];
  assert.equal(selectedIndex(seats, '/ui/areas/widgets:billing'), 1);
});

test("the inspector's close link is the one that goes back to the list", () => {
  const links = [
    'http://dibs.example.com/ui/claims/12',
    'http://dibs.example.com/ui/claims?who=alice',
  ];
  assert.equal(closeLinkIndex(links, '/ui/claims/7'), 1);
  assert.equal(closeLinkIndex(['http://dibs.example.com/ui/inbox?tab=dibs'], '/ui/inbox/3'), 0);
  assert.equal(closeLinkIndex(links, '/ui/claims'), -1);
  assert.equal(closeLinkIndex([], '/ui/claims/7'), -1);
});

const escape = { key: 'Escape', inField: false, dialogOpen: false, selected: true, defaultPrevented: false };

test('Escape closes a selected inspector', () => {
  assert.equal(closesOnKey(escape), true);
});

test('Escape is left alone in a field, under an open dialog, with nothing selected, or once handled', () => {
  assert.equal(closesOnKey({ ...escape, inField: true }), false);
  assert.equal(closesOnKey({ ...escape, dialogOpen: true }), false);
  assert.equal(closesOnKey({ ...escape, selected: false }), false);
  assert.equal(closesOnKey({ ...escape, defaultPrevented: true }), false);
  assert.equal(closesOnKey({ ...escape, key: 'Enter' }), false);
});

test('an opening asks for every region of the screen the page has, the inspector last', () => {
  assert.deepEqual(regionsToAsk(['inspector', 'list', 'rail']), ['rail', 'list', 'inspector']);
  assert.deepEqual(regionsToAsk(['list', 'inspector', 'results']), ['list', 'inspector']);
  assert.deepEqual(regionsToAsk(['rail', 'rail', 'inspector']), ['rail', 'inspector']);
  assert.deepEqual(regionsToAsk([]), []);
});

test('an opening always opens the inspector, even one the person was writing in', () => {
  assert.equal(openingAction('inspector', false), 'open');
  assert.equal(openingAction('inspector', true), 'open');
});

test('an opening refreshes the rail and the list where they were scrolled to, unless the person is using them', () => {
  assert.equal(openingAction('rail', false), 'refresh');
  assert.equal(openingAction('list', false), 'refresh');
  assert.equal(openingAction('rail', true), 'keep');
  assert.equal(openingAction('list', true), 'keep');
});
