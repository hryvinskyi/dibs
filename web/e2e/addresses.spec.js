// @ts-check
/**
 * Addresses typed into the browser: a claim, an area and a conversation each open with their
 * object in the inspector; the retired Board and Threads addresses redirect permanently to Claims,
 * the Inbox and the conversation; and the address of a claim that is not open answers 404 with
 * the whole page, its inspector saying so.
 */

import { expect, test } from './fixtures.js';
import { TITLES, seedWidgets } from './seed.js';
import { signIn } from './sign-in.js';

/**
 * The inspector of the page shown.
 * @param {import('@playwright/test').Page} page
 * @returns {import('@playwright/test').Locator}
 */
function inspector(page) {
  return page.locator('[data-region="inspector"]');
}

/**
 * Opens an address and expects its page, with an object selected in the inspector whose heading
 * reads `heading`.
 * @param {import('@playwright/test').Page} page
 * @param {string} address
 * @param {string | RegExp} heading
 * @returns {Promise<void>}
 */
async function expectInspected(page, address, heading) {
  const answer = await page.goto(address);
  expect(answer?.status()).toBe(200);
  await expect(page).toHaveURL(address);
  await expect(inspector(page)).toHaveAttribute('data-selected', 'true');
  await expect(inspector(page).locator('[data-inspector-heading]')).toHaveText(heading);
  await expect(page.locator('[data-region="list"] [data-row][aria-current="true"]')).toHaveCount(1);
}

/**
 * Opens an old address and expects it to redirect permanently to `target`.
 * @param {import('@playwright/test').Page} page
 * @param {string} address
 * @param {string} target
 * @returns {Promise<void>}
 */
async function expectRedirect(page, address, target) {
  const answer = await page.goto(address);
  expect(answer?.status()).toBe(200);
  await expect(page).toHaveURL(target);
  const redirected = await answer?.request().redirectedFrom()?.response();
  expect(redirected?.status()).toBe(308);
}

test('a claim, an area and a conversation open from their addresses with their object in the inspector', async ({
  page,
  dibs,
}) => {
  const seeded = await seedWidgets(dibs);
  await signIn(page, dibs.url, dibs.adminToken);

  await expectInspected(page, `${dibs.url}/ui/claims/${seeded.review}`, TITLES.review);
  await expectInspected(page, `${dibs.url}/ui/areas/api:billing`, /alice holds billing$/);
  await expectInspected(page, `${dibs.url}/ui/inbox/${seeded.thread}`, TITLES.thread);
});

test('the Board and Threads addresses land on Claims, the Inbox and the conversation', async ({ page, dibs }) => {
  const seeded = await seedWidgets(dibs);
  await signIn(page, dibs.url, dibs.adminToken);

  await expectRedirect(page, `${dibs.url}/ui/board`, `${dibs.url}/ui/claims`);
  await expectRedirect(page, `${dibs.url}/ui/threads`, `${dibs.url}/ui/inbox`);
  await expectRedirect(page, `${dibs.url}/ui/threads/${seeded.thread}`, `${dibs.url}/ui/inbox/${seeded.thread}`);
  await expect(inspector(page).locator('[data-inspector-heading]')).toHaveText(TITLES.thread);
});

test('the address of a claim that is not open shows the whole page, its inspector saying so', async ({
  page,
  dibs,
}) => {
  const seeded = await seedWidgets(dibs);
  await signIn(page, dibs.url, dibs.adminToken);

  // An item in the queue, which nobody claims, and an id no item has.
  for (const id of [seeded.queued, 1]) {
    const answer = await page.goto(`${dibs.url}/ui/claims/${id}`);
    expect(answer?.status()).toBe(404);
    await expect(page.locator('#rail')).toBeVisible();
    await expect(page.locator('[data-region="list"]').getByText(TITLES.working)).toBeVisible();
    await expect(inspector(page).locator('[data-inspector-heading]')).toHaveText('That claim is not open any more.');
  }
  await inspector(page).getByRole('link', { name: 'Close' }).click();
  await expect(page).toHaveURL(`${dibs.url}/ui/claims`);
  await expect(inspector(page)).toHaveAttribute('data-selected', 'false');
});
