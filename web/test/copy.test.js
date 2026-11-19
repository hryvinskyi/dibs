import assert from 'node:assert/strict';
import { test } from 'node:test';

import { copiedText } from '../src/copy.js';

/**
 * A button as far as the copy reads it.
 * @param {Record<string, string>} attributes
 */
function button(attributes) {
  return {
    /** @param {string} name */
    hasAttribute: (name) => Object.hasOwn(attributes, name),
    /** @param {string} name */
    getAttribute: (name) => attributes[name] ?? null,
  };
}

const PAGE = 'http://dibs.example.com/ui/claims/7?who=alice';

test('a copy button copies the text it carries', () => {
  assert.equal(copiedText(button({ 'data-copy': 'dibs claim gh:acme/widgets#42 --area api:billing' }), PAGE),
    'dibs claim gh:acme/widgets#42 --area api:billing');
});

test('a copy-link button copies the address of the page shown', () => {
  assert.equal(copiedText(button({ 'data-copy-link': '' }), PAGE), PAGE);
});

test('an empty copy text is copied as it is', () => {
  assert.equal(copiedText(button({ 'data-copy': '' }), PAGE), '');
});
