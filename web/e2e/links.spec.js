// @ts-check
/**
 * Every reference is a link: in the inspector of a claim in review, with a recorded branch and a
 * file it changes as another claim does, and in the inspector of a ticket in the queue, every
 * ticket, pull request, branch and file is or holds a link; the pull request links to its page on
 * GitHub; and a reference inside a message is a link.
 */

import { expect, test } from './fixtures.js';
import { seedWidgets } from './seed.js';
import { signIn } from './sign-in.js';

/** The elements that print a ticket or a pull request, a branch and a file. */
const REFERENCES = '.ref, .br, .file';

/**
 * How many references a region prints, by their class, and how many of them are no link and
 * hold none.
 * @param {import('@playwright/test').Locator} region
 * @returns {Promise<{ ref: number, br: number, file: number, unlinked: string[] }>}
 */
function references(region) {
  return region.evaluate((element, selector) => {
    const found = Array.from(element.querySelectorAll(selector));
    const linked = (/** @type {Element} */ node) =>
      node.matches('a[href]:not([href=""])') || node.querySelector('a[href]:not([href=""])') !== null;
    return {
      ref: found.filter((node) => node.classList.contains('ref')).length,
      br: found.filter((node) => node.classList.contains('br')).length,
      file: found.filter((node) => node.classList.contains('file')).length,
      unlinked: found.filter((node) => !linked(node)).map((node) => node.outerHTML),
    };
  }, REFERENCES);
}

test("a claim's inspector links its ticket, pull request, branch and the files it shares", async ({
  page,
  dibs,
}) => {
  const seeded = await seedWidgets(dibs);
  await signIn(page, dibs.url, dibs.adminToken);
  await page.goto(`${dibs.url}/ui/claims/${seeded.review}`);
  const inspector = page.locator('[data-region="inspector"]');
  await expect(inspector).toHaveAttribute('data-selected', 'true');

  const found = await references(inspector);
  expect(found.unlinked).toEqual([]);
  expect(found.ref).toBeGreaterThan(0);
  expect(found.br).toBeGreaterThan(0);
  expect(found.file).toBeGreaterThan(0);
  await expect(inspector.locator('.br a')).toHaveAttribute('href', 'https://github.com/acme/widgets/tree/bob/2-refunds');
  await expect(inspector.locator('a[href="https://github.com/acme/widgets/pull/12"]')).toHaveAttribute(
    'rel',
    'noopener noreferrer',
  );
});

test("a queued ticket's inspector links every ticket it names", async ({ page, dibs }) => {
  const seeded = await seedWidgets(dibs);
  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/queue/${seeded.queued}`);
  const inspector = page.locator('[data-region="inspector"]');
  await expect(inspector).toHaveAttribute('data-selected', 'true');

  const found = await references(inspector);
  expect(found.unlinked).toEqual([]);
  expect(found.ref).toBeGreaterThan(0);
  await expect(inspector.getByRole('link', { name: 'gh:acme/widgets#3 on the tracker' }).first()).toHaveAttribute(
    'href',
    'https://github.com/acme/widgets/issues/3',
  );
});

test('a reference in a message is a link', async ({ page, dibs }) => {
  const seeded = await seedWidgets(dibs);
  await signIn(page, dibs.url, dibs.memberToken);
  await page.goto(`${dibs.url}/ui/inbox/${seeded.thread}`);

  const body = page.locator('.msg__body').filter({ hasText: 'May I change' });
  await expect(body.getByRole('link', { name: 'gh:acme/widgets#7' })).toHaveAttribute(
    'href',
    'https://github.com/acme/widgets/issues/7',
  );
});
