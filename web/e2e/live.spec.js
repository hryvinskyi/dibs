// @ts-check
/**
 * The screens kept live in a real browser, without a reload: an item claimed through the API
 * shows on Claims; a take-over reason being typed in a silent claim's inspector stays while the
 * list follows the events, the page saying that something changed, and the inspector catches up
 * once the form is left; a click on another row while a refresh is on its way keeps the row
 * clicked; on a phone the rail stays collapsed across a refresh; a slot taken shows as held in the
 * rail; an overlap reported shows as a card on Now; a feature claim's wait for an area debt work
 * holds shows on the claim, and goes once the area is handed over; a reply in an existing
 * conversation raises the rail's Inbox count on Claims; and a branch a heartbeat records replaces
 * the claim's branch on an open Claims page.
 */

import { ageClaim } from './claim-age.js';
import {
  applySettings,
  claimFrom,
  claimItem,
  heartbeatOn,
  importItems,
  narrowClaim,
  openSession,
  postMessage,
  reportFootprint,
  startThread,
  takeSlot,
  widgetsSettings,
} from './dibs-api.js';
import { expect, test } from './fixtures.js';
import { expectKept, expectReplaced, expectSameDocument, markDocument, markElement } from './same-document.js';
import { signIn } from './sign-in.js';

/**
 * The item at `index` of an import's answer.
 * @param {readonly number[]} ids
 * @param {number} index
 * @returns {number}
 */
function itemAt(ids, index) {
  const id = ids[index];
  if (id === undefined) {
    throw new Error('the import created fewer items than asked');
  }
  return id;
}

/**
 * The list of the screen shown.
 * @param {import('@playwright/test').Page} page
 * @returns {import('@playwright/test').Locator}
 */
function list(page) {
  return page.locator('[data-region="list"]');
}

/**
 * The inspector of the screen shown.
 * @param {import('@playwright/test').Page} page
 * @returns {import('@playwright/test').Locator}
 */
function inspector(page) {
  return page.locator('[data-region="inspector"]');
}

/**
 * The page's notice that something changed.
 * @param {import('@playwright/test').Page} page
 * @returns {import('@playwright/test').Locator}
 */
function changed(page) {
  return page.getByRole('status').filter({ hasText: 'Something changed' });
}

test('an item claimed through the API shows on Claims without a reload', async ({ page, dibs }) => {
  await applySettings(dibs.url, dibs.adminToken, widgetsSettings());
  const ids = await importItems(dibs.url, dibs.adminToken, [
    { ref: 'gh:acme/widgets#4', title: 'Round the invoice totals' },
  ]);

  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/claims`);
  await expect(list(page).getByText('Round the invoice totals')).toHaveCount(0);
  await markDocument(page);

  await claimFrom(dibs.url, dibs.adminToken, itemAt(ids, 0), 'alice-laptop', { areas: ['api:billing'] });
  const row = list(page).locator('[data-row]').filter({ hasText: 'Round the invoice totals' });
  await expect(row).toBeVisible();
  await expect(row.getByRole('link', { name: 'widgets#4' })).toHaveAttribute(
    'href',
    'https://github.com/acme/widgets/issues/4',
  );
  await expect(row.getByRole('link', { name: 'billing' })).toHaveAttribute('href', '/ui/areas/api:billing');
  await expect(changed(page)).toBeHidden();
  await expectSameDocument(page);
});

test('a half-typed take-over reason survives an event while the list follows, and the inspector catches up once the form is left', async ({
  page,
  dibs,
}) => {
  await applySettings(dibs.url, dibs.adminToken, widgetsSettings());
  const ids = await importItems(dibs.url, dibs.adminToken, [
    { ref: 'gh:acme/widgets#1', title: 'Gone quiet' },
    { ref: 'gh:acme/widgets#2', title: 'Claimed while typing' },
    { ref: 'gh:acme/widgets#3', title: 'Claimed after the form' },
  ]);
  const session = await claimFrom(dibs.url, dibs.adminToken, itemAt(ids, 0), 'alice-laptop');
  ageClaim(dibs.database, itemAt(ids, 0), 25);

  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/claims/${itemAt(ids, 0)}`);
  const reason = inspector(page).getByRole('textbox', { name: 'Reason' });
  await reason.fill('no heartbeat since yesterd');
  await markDocument(page);
  await markElement(inspector(page));

  await claimItem(dibs.url, dibs.adminToken, itemAt(ids, 1), session);
  await expect(list(page).getByText('Claimed while typing')).toBeVisible();
  await expect(changed(page)).toBeVisible();
  await expect(reason).toHaveValue('no heartbeat since yesterd');
  await expect(reason).toBeFocused();
  await expectKept(inspector(page));

  await reason.fill('');
  await page.getByRole('heading', { level: 1, name: 'Claims' }).click();
  await expect(changed(page)).toBeHidden();
  await expectReplaced(inspector(page));
  await markElement(inspector(page));

  await claimItem(dibs.url, dibs.adminToken, itemAt(ids, 2), session);
  await expect(list(page).getByText('Claimed after the form')).toBeVisible();
  await expectReplaced(inspector(page));
  await expect(changed(page)).toBeHidden();
  await expectSameDocument(page);
});

test('a click on another row while a refresh is on its way keeps the row clicked', async ({ page, dibs }) => {
  await applySettings(dibs.url, dibs.adminToken, widgetsSettings());
  const ids = await importItems(dibs.url, dibs.adminToken, [
    { ref: 'gh:acme/widgets#1', title: 'Selected first' },
    { ref: 'gh:acme/widgets#2', title: 'Clicked meanwhile' },
    { ref: 'gh:acme/widgets#3', title: 'Claimed to cause a refresh' },
  ]);
  const session = await claimFrom(dibs.url, dibs.adminToken, itemAt(ids, 0), 'alice-laptop');
  await claimItem(dibs.url, dibs.adminToken, itemAt(ids, 1), session);

  await signIn(page, dibs.url, dibs.memberToken);
  const first = `${dibs.url}/ui/claims/${itemAt(ids, 0)}`;
  await page.goto(first);
  await markDocument(page);

  // The refresh of the first claim's page is held until the other row was clicked.
  /** @type {(value: void) => void} */
  let reached = () => {};
  const refreshAsked = new Promise((resolve) => {
    reached = resolve;
  });
  /** @type {(value: void) => void} */
  let release = () => {};
  const released = new Promise((resolve) => {
    release = resolve;
  });
  await page.route(first, async (route) => {
    reached();
    await released;
    await route.continue();
  });

  await claimItem(dibs.url, dibs.adminToken, itemAt(ids, 2), session);
  await refreshAsked;
  const clicked = list(page).locator('[data-row]').filter({ hasText: 'Clicked meanwhile' });
  await clicked.getByRole('link', { name: 'Clicked meanwhile' }).click();
  await expect(page).toHaveURL(`${dibs.url}/ui/claims/${itemAt(ids, 1)}`);
  await expect(inspector(page).locator('[data-inspector-heading]')).toHaveText('Clicked meanwhile');
  release();

  await expect(list(page).getByText('Claimed to cause a refresh')).toBeVisible();
  await expect(page).toHaveURL(`${dibs.url}/ui/claims/${itemAt(ids, 1)}`);
  await expect(inspector(page).locator('[data-inspector-heading]')).toHaveText('Clicked meanwhile');
  await expect(clicked).toHaveAttribute('aria-current', 'true');
  await expect(list(page).locator('[data-row][aria-current="true"]')).toHaveCount(1);
  await expectSameDocument(page);
});

test('on a phone the rail stays collapsed across a refresh', async ({ page, dibs }) => {
  await page.setViewportSize({ width: 400, height: 800 });
  await applySettings(dibs.url, dibs.adminToken, widgetsSettings());
  const ids = await importItems(dibs.url, dibs.adminToken, [{ ref: 'gh:acme/widgets#1', title: 'Claimed on a phone' }]);

  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/claims`);
  const rail = page.locator('#rail');
  const menu = rail.getByRole('button', { name: 'Menu' });
  const people = rail.locator('section').filter({ has: page.locator('#rail-people') });
  await expect(menu).toHaveAttribute('aria-expanded', 'false');
  await expect(people).toBeHidden();
  await markDocument(page);
  await markElement(rail);

  await claimFrom(dibs.url, dibs.adminToken, itemAt(ids, 0), 'alice-phone');
  await expect(list(page).getByText('Claimed on a phone')).toBeVisible();
  await expectReplaced(rail);
  await expect(rail).toHaveAttribute('data-collapsible', 'true');
  await expect(menu).toBeVisible();
  await expect(menu).toHaveAttribute('aria-expanded', 'false');
  await expect(people).toBeHidden();
  await expect(people).toContainText('alice');

  await menu.click();
  await expect(people).toBeVisible();
  await expectSameDocument(page);
});

test('a slot taken through the API shows as held in the rail without a reload', async ({ page, dibs }) => {
  await applySettings(dibs.url, dibs.adminToken, widgetsSettings({ slots: [{ name: 'deploy', max_hold: '30m' }] }));

  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/claims`);
  const slot = page.locator('#rail li').filter({ has: page.locator('[id="slot-deploy"]') });
  await expect(slot).not.toHaveClass(/\bheld\b/);
  await expect(slot.locator('summary')).toContainText('free');
  await markDocument(page);

  const session = await openSession(dibs.url, dibs.adminToken, 'alice-laptop');
  await takeSlot(dibs.url, dibs.adminToken, 'deploy', session, 'ship the e2e build');
  await expect(slot).toHaveClass(/\bheld\b/);
  await expect(slot.locator('summary')).toContainText('alice');
  await expectSameDocument(page);
});

test('an overlap reported through the API shows as a card on Now without a reload', async ({ page, dibs }) => {
  await applySettings(dibs.url, dibs.adminToken, widgetsSettings());
  const ids = await importItems(dibs.url, dibs.adminToken, [
    { ref: 'gh:acme/widgets#1', title: 'Rates in the invoice' },
    { ref: 'gh:acme/widgets#2', title: 'Rounding of the invoice' },
  ]);
  const people = [
    { token: dibs.adminToken, branch: 'alice/1-rates' },
    { token: dibs.memberToken, branch: 'bob/2-rounding' },
  ];

  await signIn(page, dibs.url, dibs.memberToken);
  const card = page.locator('article.need').filter({ hasText: 'Overlap' });
  await expect(card).toHaveCount(0);
  await markDocument(page);

  for (const [index, person] of people.entries()) {
    const item = itemAt(ids, index);
    const session = await claimFrom(dibs.url, person.token, item, `laptop-${index}`);
    await heartbeatOn(dibs.url, person.token, session, 'api', person.branch);
    const stored = await reportFootprint(dibs.url, dibs.adminToken, item, {
      repo: 'api',
      branch: person.branch,
      base: 'main',
      base_sha: 'abcd1234',
      base_at: '2026-03-01T00:00:00Z',
      paths: ['src/billing/round.rs'],
    });
    expect(stored).toBe(true);
  }
  await expect(card).toBeVisible();
  await expect(card.getByRole('link', { name: 'widgets#1', exact: true })).toBeVisible();
  await expect(card.getByRole('link', { name: 'widgets#2', exact: true })).toBeVisible();
  await expectSameDocument(page);
});

test("a feature claim's wait for an area debt work holds shows on the claim, and goes once the area is handed over", async ({
  page,
  dibs,
}) => {
  await applySettings(dibs.url, dibs.adminToken, widgetsSettings());
  const ids = await importItems(dibs.url, dibs.adminToken, [
    { ref: 'gh:acme/widgets#1', title: 'Tidy the ledger' },
    { ref: 'gh:acme/widgets#2', title: 'Currency in the ledger' },
  ]);

  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/claims`);
  await markDocument(page);
  await claimFrom(dibs.url, dibs.adminToken, itemAt(ids, 0), 'alice-laptop', { areas: ['api:billing'], kind: 'debt' });
  await claimFrom(dibs.url, dibs.memberToken, itemAt(ids, 1), 'bob-desk', { areas: ['api:billing'] });
  const feature = list(page).locator('[data-row]').filter({ hasText: 'Currency in the ledger' });
  await feature.getByRole('link', { name: 'Currency in the ledger' }).click();
  await expect(page).toHaveURL(`${dibs.url}/ui/claims/${itemAt(ids, 1)}`);
  const waiting = inspector(page)
    .locator('li')
    .filter({ hasText: /^api:billingwaiting for .*alice's debt work$/ });
  await expect(waiting).toBeVisible();

  await narrowClaim(dibs.url, dibs.adminToken, itemAt(ids, 0), ['api:billing']);
  await expect(waiting).toHaveCount(0);
  await expect(inspector(page).locator('.blk').filter({ hasText: 'Areas' })).toContainText('api:billing');
  await expectSameDocument(page);
});

test("a reply in an existing conversation raises the rail's Inbox count on Claims without a reload", async ({
  page,
  dibs,
}) => {
  await applySettings(dibs.url, dibs.adminToken, widgetsSettings());
  const thread = await startThread(dibs.url, dibs.memberToken, {
    subject: 'The invoice totals',
    to: ['alice'],
    body: 'Are they rounded per line?',
  });

  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/claims`);
  const inbox = page.locator('#rail nav a[href="/ui/inbox"] .k');
  await expect(inbox).toHaveText('0');
  await markDocument(page);

  await postMessage(dibs.url, dibs.adminToken, thread, 'Per line, then summed.');
  await expect(inbox).toHaveText('1');
  await expect(changed(page)).toBeHidden();
  await expectSameDocument(page);
});

test("a branch a heartbeat records replaces the claim's branch on an open Claims page without a reload", async ({
  page,
  dibs,
}) => {
  await applySettings(dibs.url, dibs.adminToken, widgetsSettings());
  const ids = await importItems(dibs.url, dibs.adminToken, [{ ref: 'gh:acme/widgets#1', title: 'Rates in the ledger' }]);
  const session = await claimFrom(dibs.url, dibs.memberToken, itemAt(ids, 0), 'bob-desk');
  await heartbeatOn(dibs.url, dibs.memberToken, session, 'api', 'bob/1-rates');

  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/claims/${itemAt(ids, 0)}`);
  const branch = inspector(page).locator('dt', { hasText: 'Branch' }).locator('xpath=following-sibling::dd[1]');
  await expect(branch).toContainText('bob/1-rates');
  await markDocument(page);

  await heartbeatOn(dibs.url, dibs.memberToken, session, 'api', 'bob/7-other');
  await expect(branch).toContainText('bob/7-other');
  await expect(branch).not.toContainText('bob/1-rates');
  await expectSameDocument(page);
});
