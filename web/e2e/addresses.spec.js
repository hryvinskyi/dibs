// @ts-check
/**
 * Addresses typed into the browser: a claim, an area and a conversation each open with their
 * object in the inspector; the retired Board and Threads addresses redirect permanently to Claims,
 * the Inbox and the conversation; the address of a claim that is not open answers 404 with
 * the whole page, its inspector saying so; and a page answering a form, whose address only a POST
 * answers, never asks for that address with a GET, not even when the stream says the page may have
 * missed events, its Reload keeps leading to the screen's own address, and the browser's Back from a
 * row opened in place there shows the screen rather than asking for the posted address.
 */

import { startThread } from './dibs-api.js';
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

/** The longest reply the service takes, in bytes. */
const LONGEST_REPLY = 16_384;

test('a page answering a form never asks for its address with a GET, and its Reload leads to the screen', async ({
  page,
  dibs,
}) => {
  const threadId = await startThread(dibs.url, dibs.adminToken, {
    subject: 'Rounding rules',
    to: ['bob'],
    body: 'May I touch round.rs?',
  });
  const postedTo = `/ui/threads/${threadId}/messages`;
  const screen = `/ui/inbox/${threadId}`;
  /** @type {string[]} */
  const fetchedWithGet = [];
  page.on('request', (request) => {
    if (request.method() === 'GET' && new URL(request.url()).pathname === postedTo) {
      fetchedWithGet.push(request.url());
    }
  });

  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}${screen}`);
  await page.getByRole('textbox', { name: 'Message' }).fill('x'.repeat(LONGEST_REPLY + 1));

  // The refused page's stream is refused once, and the session check then says that the log no
  // longer reaches back to the page's cursor, as it does once the retention removed those events.
  let streams = 0;
  page.on('request', (request) => {
    if (new URL(request.url()).pathname === '/ui/events/stream') {
      streams += 1;
    }
  });
  await page.route((url) => url.pathname === '/ui/events/stream', (route) => route.fulfill({ status: 410 }), {
    times: 1,
  });
  /** @type {(value: void) => void} */
  let resynced = () => {};
  const checked = new Promise((resolve) => {
    resynced = resolve;
  });
  await page.route(
    (url) => url.pathname === '/ui/session',
    async (route) => {
      const after = Number(new URL(route.request().url()).searchParams.get('after') ?? '0');
      await route.fulfill({
        status: 410,
        contentType: 'application/json',
        body: JSON.stringify({
          error: { code: 'cursor_expired', message: 'The events are gone.', details: { latest: after } },
        }),
      });
      resynced();
    },
    { times: 1 },
  );

  const sent = page.waitForResponse((response) => new URL(response.url()).pathname === postedTo);
  await page.getByRole('button', { name: 'Send' }).click();
  expect((await sent).status()).toBe(422);
  await expect(page).toHaveURL(`${dibs.url}${postedTo}`);
  await checked;
  // After the resync the stream is followed again, from the newest event.
  await expect.poll(() => streams).toBeGreaterThanOrEqual(2);
  // Longer than a burst's window, after which a refresh would have asked for the address.
  await page.waitForTimeout(1500);

  expect(fetchedWithGet).toEqual([]);
  const notice = page.locator('[data-changed]');
  await expect(notice).toBeHidden();
  await expect(notice.getByRole('link', { name: 'Reload', includeHidden: true })).toHaveAttribute('href', screen);
  await expect(page.getByRole('textbox', { name: 'Message' })).toHaveValue('x'.repeat(LONGEST_REPLY + 1));
});

test("Back from a row opened in place on a page answering a form shows the screen, never a GET of the posted address", async ({
  page,
  dibs,
}) => {
  const threadId = await startThread(dibs.url, dibs.adminToken, {
    subject: 'Rounding rules',
    to: ['bob'],
    body: 'May I touch round.rs?',
  });
  const otherId = await startThread(dibs.url, dibs.adminToken, {
    subject: 'Staging is down',
    to: ['bob'],
    body: 'Who is on it?',
  });
  const postedTo = `/ui/threads/${threadId}/messages`;
  /** @type {string[]} */
  const fetchedWithGet = [];
  page.on('request', (request) => {
    if (request.method() === 'GET' && new URL(request.url()).pathname === postedTo) {
      fetchedWithGet.push(request.url());
    }
  });

  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/inbox/${threadId}`);
  await page.getByRole('textbox', { name: 'Message' }).fill('x'.repeat(LONGEST_REPLY + 1));
  const sent = page.waitForResponse((response) => new URL(response.url()).pathname === postedTo);
  await page.getByRole('button', { name: 'Send' }).click();
  expect((await sent).status()).toBe(422);
  await expect(page).toHaveURL(`${dibs.url}${postedTo}`);

  await page.locator('[data-region="list"]').getByRole('link', { name: 'Staging is down' }).click();
  await expect(page).toHaveURL(`${dibs.url}/ui/inbox/${otherId}`);
  await expect(page.locator('[data-inspector-heading]')).toHaveText('Staging is down');

  await page.goBack();
  await expect(page.locator('[data-inspector-heading]')).toHaveText('Rounding rules');
  await expect(page.locator('[data-region="list"] [data-row][aria-current="true"]')).toContainText('Rounding rules');
  expect(fetchedWithGet).toEqual([]);
});
