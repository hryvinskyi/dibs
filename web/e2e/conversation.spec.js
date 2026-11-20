// @ts-check
/**
 * Conversations on the Inbox in a real browser: a reply posts through the form and a teammate's
 * message appears live, the conversation keeping itself current without the page's change notice
 * and over the page's one event stream; a reference in a message that arrives live becomes a link
 * without a reload while a half-written reply stays in the form; a recipient acknowledges an urgent
 * message, and one that arrives live; the author sees an acknowledgement arrive; an escalation
 * reads as from dibs and leaves Now's Arbitrate card once acknowledged; a token the server does not
 * know is refused at sign-in; and the policy watch fails a page that breaks the policy.
 */

import {
  ackMessage,
  applySettings,
  escalateItem,
  importItems,
  postMessage,
  startThread,
  widgetsSettings,
} from './dibs-api.js';
import { watchPolicy } from './csp-watch.js';
import { expect, test } from './fixtures.js';
import { expectSameDocument, markDocument } from './same-document.js';
import { signIn } from './sign-in.js';

/** The path of the page's event stream. */
const STREAM_PATH = '/ui/events/stream';

/**
 * Counts the requests to the event stream of the document shown that have neither finished nor
 * failed. A request that loads another document into the page starts the count again, since the
 * browser does not always report the end of the stream of a document it left.
 * @param {import('@playwright/test').Page} page
 * @returns {() => number} how many are open now
 */
function watchStreams(page) {
  /** @type {Set<import('@playwright/test').Request>} */
  const open = new Set();
  page.on('request', (request) => {
    if (request.isNavigationRequest() && request.frame() === page.mainFrame()) {
      open.clear();
    } else if (new URL(request.url()).pathname === STREAM_PATH) {
      open.add(request);
    }
  });
  page.on('requestfinished', (request) => open.delete(request));
  page.on('requestfailed', (request) => open.delete(request));
  return () => open.size;
}

/**
 * The messages of the conversation shown.
 * @param {import('@playwright/test').Page} page
 * @returns {import('@playwright/test').Locator}
 */
function messages(page) {
  return page.getByRole('list', { name: 'Messages, oldest first' });
}

/**
 * The page's notice that something changed.
 * @param {import('@playwright/test').Page} page
 * @returns {import('@playwright/test').Locator}
 */
function changed(page) {
  return page.getByRole('status').filter({ hasText: 'Something changed' });
}

test('a reply posts, and a message posted meanwhile appears live over the one event stream', async ({
  page,
  dibs,
}) => {
  const openStreams = watchStreams(page);
  const threadId = await startThread(dibs.url, dibs.adminToken, {
    subject: 'Rounding rules',
    to: ['bob'],
    body: 'May I touch round.rs?',
  });

  await signIn(page, dibs.url, dibs.memberToken);
  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: /^Inbox/ }).click();
  await page.waitForURL(`${dibs.url}/ui/inbox`, { waitUntil: 'load' });
  await markDocument(page);
  await page.locator('[data-region="list"]').getByRole('link', { name: 'Rounding rules' }).click();
  await expect(page).toHaveURL(`${dibs.url}/ui/inbox/${threadId}`);
  await expect(page.locator('[data-inspector-heading]')).toHaveText('Rounding rules');
  await expect(messages(page).getByText('May I touch round.rs?')).toBeVisible();
  await expectSameDocument(page);

  await page.getByRole('textbox', { name: 'Message' }).fill('Yes, go ahead.');
  await page.getByRole('button', { name: 'Send' }).click();
  await page.waitForURL(new RegExp(`/ui/inbox/${threadId}#m-\\d+$`), { waitUntil: 'load' });
  await expect(messages(page).getByText('Yes, go ahead.')).toBeVisible();
  await expect(messages(page).locator('.msg--mine')).toHaveCount(1);

  await markDocument(page);
  await postMessage(dibs.url, dibs.adminToken, threadId, 'Thanks, shipping it today.');
  await expect(messages(page).getByText('Thanks, shipping it today.')).toBeVisible();
  await expect(page.locator('[data-live-pill]')).toBeVisible();
  await expect(changed(page)).toBeHidden();
  await expect.poll(openStreams).toBe(1);
  await expectSameDocument(page);
});

test('a reference in a message that arrives live becomes a link, and a half-written reply stays', async ({
  page,
  dibs,
}) => {
  const threadId = await startThread(dibs.url, dibs.adminToken, {
    subject: 'Rounding rules',
    to: ['bob'],
    body: 'Which ticket holds the rounding?',
  });

  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/inbox/${threadId}`);
  const reply = page.getByRole('textbox', { name: 'Message' });
  await reply.fill('Half a sent');
  await markDocument(page);

  const messageId = await postMessage(dibs.url, dibs.adminToken, threadId, 'It is gh:acme/widgets#7, see there.');
  const live = page.locator(`#m-${messageId}`);
  await expect(live.locator('.msg__body')).toHaveText('It is gh:acme/widgets#7, see there.');
  await expect(live.getByRole('link', { name: 'gh:acme/widgets#7' })).toHaveAttribute(
    'href',
    'https://github.com/acme/widgets/issues/7',
  );
  await expect(reply).toHaveValue('Half a sent');
  await expect(changed(page)).toBeHidden();
  await expectSameDocument(page);
});

test('a recipient acknowledges an urgent message, and one that arrives live', async ({ page, dibs }) => {
  const threadId = await startThread(dibs.url, dibs.adminToken, {
    subject: 'Staging is down',
    to: ['bob'],
    body: 'Do not deploy until I say so.',
    importance: 'urgent',
    ack_required: true,
  });

  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/inbox`);
  const row = page.locator('[data-region="list"] [data-row]').filter({ hasText: 'Staging is down' });
  await expect(row.getByText('urgent', { exact: true })).toBeVisible();
  await row.getByRole('link', { name: 'Staging is down' }).click();
  await expect(page).toHaveURL(`${dibs.url}/ui/inbox/${threadId}`);
  const inspector = page.locator('[data-region="inspector"]');
  await expect(inspector.locator('.insp__top').getByText('urgent', { exact: true })).toBeVisible();

  const acks = inspector.getByRole('list', { name: 'Acknowledgements' });
  await expect(acks.locator('[data-ack-member="bob"]')).toHaveText(/bob \(you\) waiting$/);
  await inspector.getByRole('button', { name: 'Acknowledge' }).click();
  await page.waitForURL(new RegExp(`/ui/inbox/${threadId}#m-\\d+$`), { waitUntil: 'load' });
  await expect(acks.locator('[data-ack-member="bob"]')).toHaveText(/bob \(you\) acked \S+$/);
  await expect(inspector.getByRole('button', { name: 'Acknowledge' })).toHaveCount(0);

  const liveId = await postMessage(dibs.url, dibs.adminToken, threadId, 'Still down; confirm you are holding.', {
    importance: 'high',
    ack_required: true,
  });
  const live = page.locator(`#m-${liveId}`);
  await expect(live.getByText('high', { exact: true })).toBeVisible();
  await live.getByRole('button', { name: 'Acknowledge' }).click();
  await expect(page).toHaveURL(new RegExp(`/ui/inbox/${threadId}#m-${liveId}$`));
  await expect(page.locator(`#m-${liveId} [data-ack-member="bob"]`)).toHaveText(/bob \(you\) acked \S+$/);
});

test("the author sees a recipient's acknowledgement arrive without a reload", async ({ page, dibs }) => {
  const threadId = await startThread(dibs.url, dibs.adminToken, {
    subject: 'Freeze the release branch',
    to: ['bob'],
    body: 'Cutting the release at noon.',
  });
  const messageId = await postMessage(dibs.url, dibs.adminToken, threadId, 'No merges until I say so.', {
    ack_required: true,
  });

  await signIn(page, dibs.url, dibs.adminToken);
  await page.goto(`${dibs.url}/ui/inbox/${threadId}`);
  const asked = page.locator(`#m-${messageId}`);
  const bob = asked.getByRole('list', { name: 'Acknowledgements' }).locator('[data-ack-member="bob"]');
  await expect(bob).toHaveText(/bob waiting$/);
  await expect(asked.getByRole('button', { name: 'Acknowledge' })).toHaveCount(0);
  await markDocument(page);

  await ackMessage(dibs.url, dibs.memberToken, messageId);
  await expect(bob).toHaveText(/^✓ bob acked \S+$/);
  await expect(bob.locator('time')).toHaveAttribute('datetime', /^\d{4}-\d{2}-\d{2}T/);
  await expectSameDocument(page);
});

test("an escalation reads from dibs and leaves Now's Arbitrate card once the arbiter acknowledges it", async ({
  page,
  dibs,
}) => {
  await applySettings(dibs.url, dibs.adminToken, widgetsSettings());
  const [itemId] = await importItems(dibs.url, dibs.adminToken, [{ ref: 'gh:acme/widgets#5', title: 'Shared rates' }]);
  if (itemId === undefined) {
    throw new Error('the import created no item');
  }
  const { threadId, messageId } = await escalateItem(
    dibs.url,
    dibs.adminToken,
    itemId,
    'Two agents keep undoing each other in the rate table.',
  );

  await signIn(page, dibs.url, dibs.memberToken);
  const card = page.locator('article.need').filter({ hasText: 'Arbitrate' });
  await expect(card).toBeVisible();
  await card.locator(`a[href="/ui/inbox/${threadId}"]`).first().click();
  await expect(page).toHaveURL(`${dibs.url}/ui/inbox/${threadId}`);

  const notice = page.locator(`#m-${messageId}`);
  await expect(notice).toHaveClass(/\bmsg--system\b/);
  await expect(notice.locator('.msg__head')).toContainText('from dibs');
  await expect(notice.locator('.msg__head .mono')).toHaveCount(0);
  await expect(notice.locator('.msg__body')).toContainText('alice asks the arbiters of this project (bob)');
  await expect(notice.getByText('urgent', { exact: true })).toBeVisible();
  await notice.getByRole('button', { name: 'Acknowledge' }).click();
  await expect(page.locator(`#m-${messageId} [data-ack-member="bob"]`)).toHaveText(/bob \(you\) acked \S+$/);

  await page.goto(`${dibs.url}/ui/`);
  await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
  await expect(card).toHaveCount(0);
});

test('a token the server does not know is refused on the login page', async ({ page, dibs }) => {
  await page.goto(`${dibs.url}/ui/login`);
  await page.getByLabel('Paste your token').fill('dibs_not-a-token-of-this-server-0123456789');
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByRole('alert')).toContainText("That token wasn't recognised");
  await expect(page).toHaveURL(`${dibs.url}/ui/login`);
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
