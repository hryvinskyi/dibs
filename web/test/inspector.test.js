import assert from 'node:assert/strict';
import { test } from 'node:test';

import { closeLinkIndex, closesOnKey, isPlainClick, samePath, selectedIndex } from '../src/inspector.js';

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
