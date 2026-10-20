// @ts-check
/**
 * The conversations pages in a real browser: sign in with a token, open a conversation, reply
 * through the form, see a message posted through the API appear without a reload, acknowledge a
 * message that asks for it, and see a recipient's acknowledgement arrive without a reload.
 */

import { ackMessage, postMessage, startThread } from './dibs-api.js';
import { watchPolicy } from './csp-watch.js';
import { expect, test } from './fixtures.js';
import { signIn } from './sign-in.js';

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

test('a recipient acknowledges an urgent message, and one that arrives live, in the browser', async ({
  page,
  dibs,
}) => {
  const subject = `Staging is down ${Date.now()}`;
  const threadId = await startThread(dibs.url, dibs.adminToken, {
    subject,
    to: ['bob'],
    body: 'Do not deploy until I say so.',
    importance: 'urgent',
    ack_required: true,
  });

  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/threads`);
  const row = page.getByRole('listitem').filter({ hasText: subject });
  await expect(row.getByText('Needs your ack')).toBeVisible();
  await expect(row.getByText('Urgent')).toBeVisible();
  await row.getByRole('link', { name: subject }).click();
  await expect(page).toHaveURL(`${dibs.url}/ui/threads/${threadId}`);
  await expect(page.locator('.thread-head').getByText('Urgent')).toBeVisible();

  const acks = page.getByRole('list', { name: 'Acknowledgements' });
  await expect(acks.getByText('bob (you)')).toBeVisible();
  await expect(acks.locator('.ack__wait')).toHaveCount(1);
  await page.getByRole('button', { name: 'Acknowledge' }).click();
  await expect(page).toHaveURL(new RegExp(`/ui/threads/${threadId}#m-\\d+$`));
  await expect(acks.locator('.ack__done')).toContainText('bob (you)');
  await expect(page.getByRole('button', { name: 'Acknowledge' })).toHaveCount(0);

  // A message that asks arrives live with its badge and its button, and the button works.
  await postMessage(dibs.url, dibs.adminToken, threadId, 'Still down; confirm you are holding.', {
    importance: 'high',
    ack_required: true,
  });
  const live = page.locator('.msg').filter({ hasText: 'Still down; confirm you are holding.' });
  await expect(live.getByText('High')).toBeVisible();
  await live.getByRole('button', { name: 'Acknowledge' }).click();
  const reloaded = page.locator('.msg').filter({ hasText: 'Still down; confirm you are holding.' });
  await expect(reloaded.locator('.ack__done')).toContainText('bob (you)');
});

test("the author sees a recipient's acknowledgement arrive in the list without a reload", async ({
  page,
  dibs,
}) => {
  const subject = `Freeze the release branch ${Date.now()}`;
  const threadId = await startThread(dibs.url, dibs.adminToken, {
    subject,
    to: ['bob'],
    body: 'Cutting the release at noon.',
  });
  const messageId = await postMessage(dibs.url, dibs.adminToken, threadId, 'No merges until I say so.', {
    ack_required: true,
  });

  await signIn(page, dibs.url, dibs.adminToken);
  await page.goto(`${dibs.url}/ui/threads/${threadId}`);
  const asked = page.locator(`#m-${messageId}`);
  const acks = asked.getByRole('list', { name: 'Acknowledgements' });
  await expect(acks.locator('.ack__wait')).toContainText('bob');
  await expect(acks.locator('.ack__done')).toHaveCount(0);
  await expect(asked.getByRole('button', { name: 'Acknowledge' })).toHaveCount(0);

  await page.evaluate(() => document.body.setAttribute('data-e2e-marker', 'same-document'));
  await ackMessage(dibs.url, dibs.memberToken, messageId);
  await expect(acks.locator('.ack__done')).toHaveText(/^✓ bob acked \S+/);
  await expect(acks.locator('.ack__wait')).toHaveCount(0);
  await expect(acks.locator('.ack__done time')).toHaveAttribute('datetime', /^\d{4}-\d{2}-\d{2}T/);
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
