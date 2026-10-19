import assert from 'node:assert/strict';
import { test } from 'node:test';

import { clockText } from '../src/clock-text.js';

test('a time of the same local day shows hours and minutes', () => {
  const now = new Date(2026, 9, 19, 14, 32);
  assert.equal(clockText(new Date(2026, 9, 19, 9, 5), now), '09:05');
  assert.equal(clockText(new Date(2026, 9, 19, 0, 0), now), '00:00');
});

test('a time of another day shows its date too', () => {
  const now = new Date(2026, 9, 19, 0, 10);
  assert.equal(clockText(new Date(2026, 9, 18, 23, 59), now), '2026-10-18 23:59');
  assert.equal(clockText(new Date(2025, 9, 19, 8, 0), now), '2025-10-19 08:00');
});
