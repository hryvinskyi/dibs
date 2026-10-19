// @ts-check
/**
 * The conversations pages in a real browser: sign in with a token, open a conversation, reply
 * through the form, and see a message posted through the API appear without a reload.
 */

import { postMessage, startThread } from './dibs-api.js';
import { watchPolicy } from './csp-watch.js';
import { expect, test } from './fixtures.js';

/**
 * Signs in on the login page with `token`.
 * @param {import('@playwright/test').Page} page
 * @param {string} baseUrl
 * @param {string} token
 * @returns {Promise<void>}
 */
async function signIn(page, baseUrl, token) {
  await page.goto(`${baseUrl}/ui/login`);
  await page.getByLabel('Paste your token').fill(token);
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page).toHaveURL(`${baseUrl}/ui/`);
}

test('a member signs in, replies in a conversation and sees a new message arrive live', async ({
  page,
  dibs,
}) => {
  const subject = `Rounding rules ${Date.now()}`;
  const threadId = await startThread(dibs.url, dibs.adminToken, {
    subject,
    to: ['bob'],
    body: 'May I touch round.rs?',
  });

  await signIn(page, dibs.url, dibs.memberToken);
  await page.getByRole('link', { name: 'Threads' }).click();
  await page.getByRole('link', { name: subject }).click();
  await expect(page).toHaveURL(`${dibs.url}/ui/threads/${threadId}`);
  await expect(page.getByRole('heading', { name: subject })).toBeVisible();
  await expect(page.getByText('May I touch round.rs?')).toBeVisible();

  await page.getByRole('textbox', { name: 'Message' }).fill('Yes, go ahead.');
  await page.getByRole('button', { name: 'Send' }).click();
  const messages = page.getByRole('list', { name: /Messages/ });
  await expect(messages.getByText('Yes, go ahead.')).toBeVisible();
  await expect(messages.locator('.msg--mine')).toHaveCount(1);

  // A message posted through the API reaches the open page; the marker proves it was not reloaded.
  await page.evaluate(() => document.body.setAttribute('data-e2e-marker', 'same-document'));
  await postMessage(dibs.url, dibs.adminToken, threadId, 'Thanks, shipping it today.');
  await expect(messages.getByText('Thanks, shipping it today.')).toBeVisible();
  await expect(page.locator('body')).toHaveAttribute('data-e2e-marker', 'same-document');
});

test('a token the server does not know is refused on the login page', async ({ page, dibs }) => {
  await page.goto(`${dibs.url}/ui/login`);
  await page.getByLabel('Paste your token').fill('dibs_not-a-token-of-this-server-0123456789');
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByRole('alert')).toContainText("That token wasn't recognised");
});

test('the policy watch sees a script the policy forbids', async ({ browser, dibs }) => {
  const page = await browser.newPage();
  try {
    const policy = await watchPolicy(page);
    await page.goto(`${dibs.url}/ui/login`);
    await page.evaluate(() => {
      const script = document.createElement('script');
      script.textContent = 'document.title = "ran"';
      document.head.append(script);
    });
    await expect.poll(() => policy.violations().length).toBeGreaterThan(0);
    expect(await page.title()).not.toBe('ran');
  } finally {
    await page.close();
  }
});
