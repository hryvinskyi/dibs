import assert from 'node:assert/strict';
import { test } from 'node:test';

test('pure modules run under Node, where no document exists', () => {
  assert.equal(typeof globalThis.document, 'undefined');
});
