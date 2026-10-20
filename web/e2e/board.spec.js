// @ts-check
/**
 * The board in a real browser: an item imported through the API shows in Ready, a claim made
 * through the API moves it to In progress without a reload and the page's updated time with it,
 * a take-over reason the person is typing survives the events that arrive meanwhile, the board
 * catching up once the form is left, and a slot taken through the API shows as held in the Slots
 * strip without a reload.
 */

import { ageClaim } from './claim-age.js';
import { applySettings, claimItem, importItems, openSession, takeSlot } from './dibs-api.js';
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
