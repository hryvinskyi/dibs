// @ts-check
/**
 * The board in a real browser: an item imported through the API shows in Ready, a claim made
 * through the API moves it to In progress without a reload and the page's updated time with it,
 * a take-over reason the person is typing survives the events that arrive meanwhile, the board
 * catching up once the form is left, a slot taken through the API shows as held in the Slots
 * strip without a reload, an overlap of two members' branches an integration reports shows
 * as a banner without a reload, and a feature claim's wait for an area held by debt work shows,
 * and goes once the area is handed over, without a reload.
 */

import { ageClaim } from './claim-age.js';
import {
  applySettings,
  claimItem,
  heartbeatOn,
  importItems,
  narrowClaim,
  openSession,
  reportFootprint,
  takeSlot,
} from './dibs-api.js';
import { expect, test } from './fixtures.js';
import { signIn } from './sign-in.js';

/**
 * A column of the board, by its heading.
 * @param {import('@playwright/test').Page} page
 * @param {string} name
 * @returns {import('@playwright/test').Locator}
 */
function column(page, name) {
  return page.getByRole('region', { name, exact: true });
}

/**
 * Marks the open document, so that a case can tell it was not reloaded.
 * @param {import('@playwright/test').Page} page
 * @returns {Promise<void>}
 */
async function markDocument(page) {
  await page.evaluate(() => document.body.setAttribute('data-e2e-marker', 'same-document'));
}

/**
 * @param {import('@playwright/test').Page} page
 * @returns {Promise<void>}
 */
async function expectSameDocument(page) {
  await expect(page.locator('body')).toHaveAttribute('data-e2e-marker', 'same-document');
}

test('an imported item shows in Ready and moves to In progress when it is claimed, without a reload', async ({
  page,
  dibs,
}) => {
  const number = Date.now();
  const ref = `gh:acme/widgets#${number}`;
  const title = `Round the invoice totals ${number}`;
  const [itemId] = await importItems(dibs.url, dibs.adminToken, [{ ref, title, priority: 1 }]);
  if (itemId === undefined) {
    throw new Error('the import created no item');
  }

  await signIn(page, dibs.url, dibs.memberToken);
  await page.getByRole('link', { name: 'Board' }).click();
  await expect(page).toHaveURL(`${dibs.url}/ui/board`);
  const ready = column(page, 'Ready');
  const progress = column(page, 'In progress');
  await expect(ready.getByText(title)).toBeVisible();
  await expect(ready.getByRole('link', { name: ref })).toHaveAttribute(
    'href',
    `https://github.com/acme/widgets/issues/${number}`,
  );
  await expect(progress.getByText(title)).toHaveCount(0);

  const updated = page.locator('.page-head [data-updated-slot] time');
  const loadedAt = await updated.getAttribute('datetime');
  if (loadedAt === null) {
    throw new Error('the page shows no time it was updated at');
  }

  await markDocument(page);
  const session = await openSession(dibs.url, dibs.adminToken, 'laptop');
  await claimItem(dibs.url, dibs.adminToken, itemId, session);
  await expect(progress.getByText(title)).toBeVisible();
  await expect
    .poll(async () => Date.parse((await updated.getAttribute('datetime')) ?? ''))
    .toBeGreaterThan(Date.parse(loadedAt));
  await expect(updated).toHaveText(/^\d{2}:\d{2}$/);
  await expect(progress.locator('.card').filter({ hasText: title })).toContainText('alice');
  await expect(progress.getByRole('link', { name: ref })).toHaveAttribute('rel', 'noopener noreferrer');
  await expect(ready.getByText(title)).toHaveCount(0);
  await expect(page.getByRole('status').filter({ hasText: 'Something changed' })).toBeHidden();
  await expectSameDocument(page);
});

test('a half-typed take-over reason survives an event, and the board catches up once the form is left', async ({
  page,
  dibs,
}) => {
  const number = Date.now();
  const [silent, first, second] = await importItems(dibs.url, dibs.adminToken, [
    { ref: `gh:acme/widgets#${number}1`, title: `Gone quiet ${number}` },
    { ref: `gh:acme/widgets#${number}2`, title: `Claimed while typing ${number}` },
    { ref: `gh:acme/widgets#${number}3`, title: `Claimed after the form ${number}` },
  ]);
  if (silent === undefined || first === undefined || second === undefined) {
    throw new Error('the import created fewer items');
  }
  const session = await openSession(dibs.url, dibs.adminToken, 'laptop');
  await claimItem(dibs.url, dibs.adminToken, silent, session);
  ageClaim(dibs.database, silent, 25);

  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/board`);
  const stale = column(page, 'Stale');
  const progress = column(page, 'In progress');
  const card = stale.locator('.card').filter({ hasText: `Gone quiet ${number}` });
  await expect(card).toBeVisible();
  await card.getByText('Take over', { exact: true }).click();
  const reason = card.getByRole('textbox', { name: 'Reason' });
  await reason.fill('no heartbeat since yesterd');

  await markDocument(page);
  await claimItem(dibs.url, dibs.adminToken, first, session);
  const changed = page.getByRole('status').filter({ hasText: 'Something changed' });
  await expect(changed).toBeVisible();
  await expect(reason).toHaveValue('no heartbeat since yesterd');
  await expect(reason).toBeFocused();
  await expect(progress.getByText(`Claimed while typing ${number}`)).toHaveCount(0);

  await reason.fill('');
  await card.getByText('Take over', { exact: true }).click();
  await page.getByRole('heading', { name: 'Board' }).click();
  await claimItem(dibs.url, dibs.adminToken, second, session);
  await expect(progress.getByText(`Claimed after the form ${number}`)).toBeVisible();
  await expect(progress.getByText(`Claimed while typing ${number}`)).toBeVisible();
  await expect(changed).toBeHidden();
  await expectSameDocument(page);
});

test('a slot taken through the API shows as held in the Slots strip without a reload', async ({ page, dibs }) => {
  const slot = `deploy:e2e-${Date.now()}`;
  await applySettings(dibs.url, dibs.adminToken, { slots: [{ name: slot, max_hold: '30m' }] });

  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/board`);
  const strip = page.getByRole('region', { name: 'Slots', exact: true });
  const card = strip.locator(`[data-slot="${slot}"]`);
  await expect(card).toContainText('Free');

  await markDocument(page);
  const session = await openSession(dibs.url, dibs.adminToken, 'laptop');
  await takeSlot(dibs.url, dibs.adminToken, slot, session, 'ship the e2e build');
  await expect(card).toContainText('Held');
  await expect(card).toContainText('alice');
  await expect(card).toContainText('ship the e2e build');
  await expect(card.locator('time[data-clock]')).toHaveText(/^\d{2}:\d{2}$/);
  await expectSameDocument(page);
});

test('an overlap of two branches reported through the API shows as a banner without a reload', async ({
  page,
  dibs,
}) => {
  const number = Date.now();
  const shared = `config/e2e-${number}.yaml`;
  const refs = [`gh:acme/widgets#${number}`, `gh:acme/widgets#${number + 1}`];
  const ids = await importItems(
    dibs.url,
    dibs.adminToken,
    refs.map((ref) => ({ ref, title: `Overlapping work ${ref}` })),
  );
  const people = [
    { token: dibs.adminToken, branch: `alice/${number}` },
    { token: dibs.memberToken, branch: `bob/${number}` },
  ];

  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/board`);
  await markDocument(page);
  for (const [index, person] of people.entries()) {
    const itemId = ids[index];
    if (itemId === undefined) {
      throw new Error('the import created fewer items than asked');
    }
    const session = await openSession(dibs.url, person.token, `laptop-${index}`);
    await claimItem(dibs.url, person.token, itemId, session);
    await heartbeatOn(dibs.url, person.token, session, 'default', person.branch);
    const stored = await reportFootprint(dibs.url, dibs.adminToken, itemId, {
      repo: 'default',
      branch: person.branch,
      base: 'main',
      base_sha: 'abcd1234',
      base_at: '2026-03-01T00:00:00Z',
      paths: [shared],
    });
    expect(stored).toBe(true);
  }
  const banner = page.locator('[data-overlap]', { hasText: shared });
  await expect(banner).toContainText(`${refs[0]} (alice) and ${refs[1]} (bob) change the same files`);
  await expectSameDocument(page);
});

test('a feature claim waiting for an area debt work holds shows the wait until the handover, without a reload', async ({
  page,
  dibs,
}) => {
  const number = Date.now();
  const name = `ledger${number}`;
  const area = `default:${name}`;
  await applySettings(dibs.url, dibs.adminToken, { areas: [{ name, paths: [`packages/${name}/**`] }] });
  const [debtId, featureId] = await importItems(dibs.url, dibs.adminToken, [
    { ref: `gh:acme/widgets#${number}`, title: `Tidy the ledger ${number}` },
    { ref: `gh:acme/widgets#${number + 1}`, title: `Currency in the ledger ${number}` },
  ]);
  if (debtId === undefined || featureId === undefined) {
    throw new Error('the import created fewer items than asked');
  }

  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/board`);
  await markDocument(page);
  const progress = column(page, 'In progress');
  const aliceSession = await openSession(dibs.url, dibs.adminToken, 'laptop-debt');
  await claimItem(dibs.url, dibs.adminToken, debtId, aliceSession, { areas: [area], kind: 'debt' });
  const debtCard = progress.locator('.card').filter({ hasText: `Tidy the ledger ${number}` });
  await expect(debtCard.locator('[data-debt]')).toHaveText('Debt');

  const bobSession = await openSession(dibs.url, dibs.memberToken, 'laptop-feature');
  await claimItem(dibs.url, dibs.memberToken, featureId, bobSession, { areas: [area] });
  const featureCard = progress.locator('.card').filter({ hasText: `Currency in the ledger ${number}` });
  await expect(featureCard.locator('[data-waiting]')).toHaveText(`waiting for ${area} from alice`);
  await expect(featureCard.locator('[data-debt]')).toHaveCount(0);

  await narrowClaim(dibs.url, dibs.adminToken, debtId, [area]);
  await expect(featureCard.locator('[data-waiting]')).toHaveCount(0);
  await expect(featureCard).toContainText(area);
  await expectSameDocument(page);
});
