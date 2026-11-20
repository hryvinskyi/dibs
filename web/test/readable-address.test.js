import assert from 'node:assert/strict';
import { test } from 'node:test';

import { readableAddress, reloadTarget } from '../src/readable-address.js';

const origin = 'http://dibs.example.com';
const postedTo = `${origin}/ui/threads/7/messages`;
const screen = `${origin}/ui/inbox/7`;

test('a page is read again from the address it shows', () => {
  assert.equal(
    readableAddress({ answersForm: false, shown: `${screen}#m-42`, loadedAt: screen, rendered: screen }),
    `${screen}#m-42`,
  );
});

test('a page answering a form is read from the screen the server rendered, never from the address posted to', () => {
  assert.equal(readableAddress({ answersForm: true, shown: postedTo, loadedAt: postedTo, rendered: screen }), screen);
  assert.equal(
    readableAddress({ answersForm: true, shown: `${postedTo}#m-42`, loadedAt: postedTo, rendered: screen }),
    screen,
  );
});

test('a page answering a form that has navigated in place since is read from the address it shows now', () => {
  const other = `${origin}/ui/inbox/9?tab=people`;
  assert.equal(readableAddress({ answersForm: true, shown: other, loadedAt: postedTo, rendered: screen }), other);
});

test('a page whose readable address is the one it shows reloads it', () => {
  assert.equal(reloadTarget(screen, screen), null);
  assert.equal(reloadTarget(screen, `${screen}#m-42`), null);
});

test('a page answering a form goes to the screen the server rendered instead of posting the form again', () => {
  assert.equal(reloadTarget(screen, postedTo), screen);
});

test('Back to the address a form posted to leads to the screen, after rows were opened in place', () => {
  const row = `${origin}/ui/inbox/9`;
  const page = { answersForm: true, loadedAt: postedTo, rendered: screen };
  assert.equal(readableAddress({ ...page, shown: row }), row);
  assert.equal(readableAddress({ ...page, shown: postedTo }), screen);
});

test('a page that does not answer a form is read from the address Back lands on', () => {
  const page = { answersForm: false, loadedAt: screen, rendered: screen };
  assert.equal(readableAddress({ ...page, shown: `${origin}/ui/inbox/9` }), `${origin}/ui/inbox/9`);
  assert.equal(readableAddress({ ...page, shown: screen }), screen);
});
