import assert from 'node:assert/strict';
import { test } from 'node:test';

import { answerStillWanted } from '../src/answer-address.js';

test('an answer is still wanted only while the page shows the address it was fetched for', () => {
  const at = 'http://dibs.example.com/ui/claims/7?who=alice';
  assert.equal(answerStillWanted(at, at), true);
  assert.equal(answerStillWanted(at, `${at}#notes`), true, 'only the fragment differs');
  assert.equal(answerStillWanted(at, 'http://dibs.example.com/ui/claims/7?who=bob'), false, 'another query');
  assert.equal(answerStillWanted(at, 'http://dibs.example.com/ui/claims/7'), false, 'no query');
  assert.equal(answerStillWanted(at, 'http://dibs.example.com/ui/claims/8?who=alice'), false, 'another path');
  assert.equal(answerStillWanted(at, 'http://other.example.com/ui/claims/7?who=alice'), false, 'another origin');
  assert.equal(answerStillWanted(at, 'not an address'), false);
});
