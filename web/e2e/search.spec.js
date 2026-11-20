// @ts-check
/**
 * The search palette in a real browser: the command key with K and `/` open it, and so does typing
 * in the rail's search box, while tabbing onto that box does not; `Escape` closes it and gives the
 * focus back to where it was; a pull request's number finds the pull request first, its outside
 * link going to GitHub, and Enter opens its claim; a word of a claim's title finds the claim.
 */

import { expect, test } from './fixtures.js';
import { TITLES, seedWidgets } from './seed.js';
import { signIn } from './sign-in.js';

/**
 * The palette's dialog.
 * @param {import('@playwright/test').Page} page
 * @returns {import('@playwright/test').Locator}
 */
function palette(page) {
  return page.locator('dialog[data-palette]');
}

/**
 * The rail's search box.
 * @param {import('@playwright/test').Page} page
 * @returns {import('@playwright/test').Locator}
 */
function railSearch(page) {
  return page.locator('#rail form[data-search]').getByRole('searchbox', { name: 'Search everything' });
}

/**
 * Presses Tab, from the element focused, until `target` has the focus.
 * @param {import('@playwright/test').Page} page
 * @param {import('@playwright/test').Locator} target
 * @returns {Promise<void>}
 */
async function tabTo(page, target) {
  for (let presses = 0; presses < 5; presses += 1) {
    await page.keyboard.press('Tab');
    if (await target.evaluate((element) => element === document.activeElement)) {
      return;
    }
  }
  throw new Error('Tab did not reach the element');
}

test('the command key with K and / open the palette, and Escape gives the focus back', async ({ page, dibs }) => {
  await seedWidgets(dibs);
  await signIn(page, dibs.url, dibs.adminToken);
  await page.goto(`${dibs.url}/ui/claims`);
  const heading = page.getByRole('heading', { level: 1 });

  await heading.focus();
  await page.keyboard.press('ControlOrMeta+k');
  await expect(palette(page)).toBeVisible();
  await expect(page.locator('input[data-palette-input]')).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(palette(page)).toBeHidden();
  await expect(heading).toBeFocused();

  await page.keyboard.press('/');
  await expect(palette(page)).toBeVisible();
  await expect(page.locator('input[data-palette-input]')).toHaveValue('');
  await page.keyboard.press('Escape');
  await expect(palette(page)).toBeHidden();
  await expect(heading).toBeFocused();
});

test("typing in the rail's search box opens the palette, tabbing onto it does not, and a title's word finds its claim", async ({
  page,
  dibs,
}) => {
  await seedWidgets(dibs);
  await signIn(page, dibs.url, dibs.adminToken);
  await page.goto(`${dibs.url}/ui/claims`);

  await page.locator('#rail .brand').getByRole('link', { name: 'dibs' }).focus();
  await tabTo(page, railSearch(page));
  await expect(palette(page)).toBeHidden();

  await page.keyboard.type('refund');
  await expect(palette(page)).toBeVisible();
  const input = page.locator('input[data-palette-input]');
  await expect(input).toBeFocused();
  await expect(input).toHaveValue('refund');
  await expect(palette(page).locator('[data-hit]').filter({ hasText: TITLES.review }).first()).toBeVisible();

  await page.keyboard.press('Escape');
  await expect(palette(page)).toBeHidden();
  await expect(railSearch(page)).toBeFocused();
  await expect(railSearch(page)).toHaveValue('');
});

test('a palette opened again before its closing is handled keeps what is typed in it', async ({ page, dibs }) => {
  await seedWidgets(dibs);
  await signIn(page, dibs.url, dibs.adminToken);
  await page.goto(`${dibs.url}/ui/claims`);
  await page.keyboard.press('ControlOrMeta+k');
  await expect(palette(page)).toBeVisible();

  // Escape, `/` and the typing reach the page in one task, as keys queued behind a busy page do, so
  // the palette's closing is handled only once it is open again.
  await page.evaluate(() => {
    /** @param {string} key */
    const press = (key) =>
      document.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }));
    press('Escape');
    press('/');
    const field = document.querySelector('input[data-palette-input]');
    if (!(field instanceof HTMLInputElement)) {
      throw new Error('the palette has no input');
    }
    field.value = 'refund';
    field.dispatchEvent(new Event('input', { bubbles: true }));
  });

  const input = page.locator('input[data-palette-input]');
  await expect(palette(page)).toBeVisible();
  await expect(input).toHaveValue('refund');
  await expect(input).toBeFocused();
  await expect(palette(page).locator('[data-hit]').filter({ hasText: TITLES.review }).first()).toBeVisible();
});

test("a pull request's number finds it first, its outside link is GitHub's, and Enter opens its claim", async ({
  page,
  dibs,
}) => {
  const seeded = await seedWidgets(dibs);
  await signIn(page, dibs.url, dibs.adminToken);

  await page.keyboard.press('ControlOrMeta+k');
  await page.locator('input[data-palette-input]').fill('12');
  const first = palette(page).locator('[data-hit]').first();
  await expect(first).toContainText('acme/widgets#12');
  await expect(first).toHaveAttribute('aria-selected', 'true');
  await expect(palette(page).locator('[data-hit][aria-selected="true"] a[data-hit-out]')).toHaveAttribute(
    'href',
    'https://github.com/acme/widgets/pull/12',
  );

  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(`${dibs.url}/ui/claims/${seeded.review}`);
  await expect(page.locator('[data-region="inspector"] [data-inspector-heading]')).toHaveText(TITLES.review);
});
