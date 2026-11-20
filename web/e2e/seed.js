// @ts-check
/**
 * The project most cases look at, made through the API: the settings of `widgets`; alice's claim
 * on `gh:acme/widgets#1` holding `api:billing`, on the branch `alice/1-rates`; bob's claim on
 * `gh:acme/widgets#2`, on the branch `bob/2-refunds` and in review with the pull request
 * `acme/widgets#12`; both branches changing `src/billing/round.rs`; `gh:acme/widgets#3` in the
 * queue, naming `api:billing` among the areas it will touch; and a conversation alice started
 * with bob whose first message names `gh:acme/widgets#7`.
 */

import {
  applySettings,
  claimFrom,
  heartbeatOn,
  importItems,
  reportFootprint,
  startReview,
  startThread,
  widgetsSettings,
} from './dibs-api.js';

/** The file both seeded branches change. */
export const SHARED_FILE = 'src/billing/round.rs';

/**
 * What the seed made, by the part a case looks at.
 * @typedef {object} Seeded
 * @property {number} working alice's claim, working, holding `api:billing`
 * @property {number} review bob's claim, in review with `acme/widgets#12`
 * @property {number} queued the item in the queue that will touch `api:billing`
 * @property {number} thread the conversation alice started with bob
 */

/** The seeded items' titles, by the part a case looks at. */
export const TITLES = Object.freeze({
  working: 'Round the invoice totals',
  review: 'Refund the duplicate charges',
  queued: 'Search invoices by customer',
  thread: 'Rounding rules',
});

/**
 * The item at `index` of an import's answer.
 * @param {readonly number[]} ids
 * @param {number} index
 * @returns {number}
 */
function idAt(ids, index) {
  const id = ids[index];
  if (id === undefined) {
    throw new Error('the import created fewer items than asked');
  }
  return id;
}

/**
 * Seeds the project of a running server.
 * @param {import('./server-process.js').RunningServer} dibs
 * @returns {Promise<Seeded>}
 */
export async function seedWidgets(dibs) {
  await applySettings(dibs.url, dibs.adminToken, widgetsSettings());
  const ids = await importItems(dibs.url, dibs.adminToken, [
    { ref: 'gh:acme/widgets#1', title: TITLES.working, priority: 1 },
    { ref: 'gh:acme/widgets#2', title: TITLES.review, priority: 2 },
    { ref: 'gh:acme/widgets#3', title: TITLES.queued, priority: 1, areas_hint: ['api:billing'] },
  ]);
  const working = idAt(ids, 0);
  const review = idAt(ids, 1);
  const queued = idAt(ids, 2);

  const claims = [
    { token: dibs.adminToken, item: working, host: 'alice-laptop', branch: 'alice/1-rates', areas: ['api:billing'] },
    { token: dibs.memberToken, item: review, host: 'bob-desk', branch: 'bob/2-refunds', areas: [] },
  ];
  for (const claim of claims) {
    const session = await claimFrom(dibs.url, claim.token, claim.item, claim.host, { areas: claim.areas });
    await heartbeatOn(dibs.url, claim.token, session, 'api', claim.branch);
    const stored = await reportFootprint(dibs.url, dibs.adminToken, claim.item, {
      repo: 'api',
      branch: claim.branch,
      base: 'main',
      base_sha: 'abcd1234',
      base_at: '2026-03-01T00:00:00Z',
      paths: [SHARED_FILE],
    });
    if (!stored) {
      throw new Error(`the footprint of ${claim.branch} was not stored`);
    }
  }
  await startReview(dibs.url, dibs.memberToken, review, 'acme/widgets#12');

  const thread = await startThread(dibs.url, dibs.adminToken, {
    subject: TITLES.thread,
    to: ['bob'],
    body: 'May I change src/billing/round.rs for gh:acme/widgets#7?',
  });
  return { working, review, queued, thread };
}
