// @ts-check
/**
 * The inspector on Claims in a real browser: a click on a row shows its claim without a page load,
 * changes the address, puts the focus on the inspector's heading and leaves the list where it was
 * scrolled to; the browser's Back shows the claim selected before; `Escape` closes the inspector;
 * and `Copy link` copies the address of the claim shown.
 */

import { applySettings, claimItem, importItems, openSession, widgetsSettings } from './dibs-api.js';
import { expect, test } from './fixtures.js';
import { signIn } from './sign-in.js';

/** Enough claims for the list to scroll in a window 600 pixels high. */
const CLAIMS = 24;

test.use({ viewport: { width: 1680, height: 600 } });

/**
 * The title of the claim at `index`.
 * @param {number} index
 * @returns {string}
 */
function titleOf(index) {
  return `Rework the invoice export, part ${index + 1} of ${CLAIMS}`;
}

/**
 * alice's claims, one per title, from one session.
 * @param {import('./server-process.js').RunningServer} dibs
 * @returns {Promise<number[]>} the items, in the order of their titles
 */
async function seedClaims(dibs) {
  await applySettings(dibs.url, dibs.adminToken, widgetsSettings());
  const ids = await importItems(
    dibs.url,
    dibs.adminToken,
    Array.from({ length: CLAIMS }, (_, index) => ({ ref: `gh:acme/widgets#${index + 1}`, title: titleOf(index) })),
  );
  const session = await openSession(dibs.url, dibs.adminToken, 'alice-laptop');
  for (const id of ids) {
    await claimItem(dibs.url, dibs.adminToken, id, session);
  }
  return ids;
}

/**
 * The row of a claim on the list, by its title.
 * @param {import('@playwright/test').Page} page
 * @param {string} title
 * @returns {import('@playwright/test').Locator}
 */
function row(page, title) {
  return page.locator('[data-region="list"] [data-row]').filter({ hasText: title });
}

/**
 * The inspector's heading.
 * @param {import('@playwright/test').Page} page
 * @returns {import('@playwright/test').Locator}
 */
function heading(page) {
  return page.locator('[data-region="inspector"] [data-inspector-heading]');
}

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

test('a click on a row shows its claim in place, with the focus on its heading and the list where it was', async ({
  page,
  dibs,
}) => {
  const ids = await seedClaims(dibs);
  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/claims`);
  const list = page.locator('[data-region="list"]');
  const chosen = CLAIMS - 4;
  const target = row(page, titleOf(chosen));
  await target.scrollIntoViewIfNeeded();
  const scrolled = await list.evaluate((element) => element.scrollTop);
  expect(scrolled).toBeGreaterThan(0);
  await page.evaluate(() => Reflect.set(window, 'e2eStillHere', 'yes'));

  await target.getByRole('link', { name: titleOf(chosen) }).click();
  await expect(page).toHaveURL(`${dibs.url}/ui/claims/${itemAt(ids, chosen)}`);
  await expect(heading(page)).toHaveText(titleOf(chosen));
  await expect(heading(page)).toBeFocused();
  await expect(target).toHaveAttribute('aria-current', 'true');
  expect(await page.evaluate(() => Reflect.get(window, 'e2eStillHere'))).toBe('yes');
  expect(await list.evaluate((element) => element.scrollTop)).toBe(scrolled);
});

test("the browser's Back shows the claim selected before, and Escape closes the inspector", async ({
  page,
  dibs,
}) => {
  const ids = await seedClaims(dibs);
  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/claims`);

  await row(page, titleOf(0)).getByRole('link', { name: titleOf(0) }).click();
  await expect(heading(page)).toHaveText(titleOf(0));
  await row(page, titleOf(1)).getByRole('link', { name: titleOf(1) }).click();
  await expect(heading(page)).toHaveText(titleOf(1));
  await expect(page).toHaveURL(`${dibs.url}/ui/claims/${itemAt(ids, 1)}`);

  await page.goBack();
  await expect(page).toHaveURL(`${dibs.url}/ui/claims/${itemAt(ids, 0)}`);
  await expect(heading(page)).toHaveText(titleOf(0));
  await expect(row(page, titleOf(0))).toHaveAttribute('aria-current', 'true');
  await expect(row(page, titleOf(1))).not.toHaveAttribute('aria-current', 'true');

  await page.keyboard.press('Escape');
  await expect(page).toHaveURL(`${dibs.url}/ui/claims`);
  await expect(page.locator('[data-region="inspector"]')).toHaveAttribute('data-selected', 'false');
  await expect(row(page, titleOf(0)).getByRole('link', { name: titleOf(0) })).toBeFocused();
});

test('Copy link copies the address of the claim shown', async ({ page, context, dibs }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: dibs.url });
  const ids = await seedClaims(dibs);
  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/claims`);

  await row(page, titleOf(2)).getByRole('link', { name: titleOf(2) }).click();
  const address = `${dibs.url}/ui/claims/${itemAt(ids, 2)}`;
  await expect(page).toHaveURL(address);
  await page.locator('[data-region="inspector"]').getByRole('button', { name: 'Copy link' }).click();
  await expect(page.locator('[data-toast]')).toHaveText('Copied');
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(address);
});
