// @ts-check
/**
 * Every screen in a real browser, in the light theme and the dark one, at 1680, 1100 and 400
 * pixels wide: the page answers, its entry in the navigation is the current one (Search has
 * none), the page does not scroll sideways, the main landmark and the skip link are there, IBM
 * Plex Sans is loaded, and the page reports no policy violation. At 1100 pixels a selected
 * object's inspector lies over the list and its close link goes back to the list; at 400 pixels
 * the rail's Menu button opens and closes the rail's sections, and wider it is hidden. A list's
 * field to narrow it holds its whole placeholder, and spans the list on a phone. On a window too
 * short for the rail, the rail scrolls and its search box keeps its height.
 */

import { expect, test } from './fixtures.js';
import { seedWidgets } from './seed.js';
import { signIn } from './sign-in.js';

/**
 * A screen as a case looks at it: where it is, the address of its list in the navigation (none on
 * Search), and the address of an object it shows in its inspector (none where it has no
 * inspector).
 * @typedef {object} ScreenCase
 * @property {string} name
 * @property {string} address
 * @property {string | null} nav the path of its navigation entry
 * @property {((seeded: import('./seed.js').Seeded) => string) | null} inspect
 */

/** @type {readonly ScreenCase[]} */
const SCREENS = [
  { name: 'Now', address: '/ui/', nav: '/ui/', inspect: null },
  { name: 'Claims', address: '/ui/claims', nav: '/ui/claims', inspect: (seeded) => `/ui/claims/${seeded.review}` },
  { name: 'Queue', address: '/ui/queue', nav: '/ui/queue', inspect: (seeded) => `/ui/queue/${seeded.queued}` },
  { name: 'Areas', address: '/ui/areas', nav: '/ui/areas', inspect: () => '/ui/areas/api:billing' },
  { name: 'Inbox', address: '/ui/inbox', nav: '/ui/inbox', inspect: (seeded) => `/ui/inbox/${seeded.thread}` },
  { name: 'Search', address: '/ui/search?q=widgets', nav: null, inspect: null },
];

/** The screens whose header holds no field to narrow the list with. */
const WITHOUT_FILTER = new Set(['Now']);

/** @type {readonly ('light' | 'dark')[]} */
const THEMES = ['light', 'dark'];

/** The widths the screens are drawn at, in pixels. */
const WIDTHS = [1680, 1100, 400];

/** The width at which a selected object's inspector lies over the list. */
const NARROW_SPLIT = 1100;

/** The width at which the rail becomes a menu. */
const PHONE = 400;

/**
 * Whether the page is wider than the window, so that it scrolls sideways.
 * @param {import('@playwright/test').Page} page
 * @returns {Promise<boolean>}
 */
function scrollsSideways(page) {
  return page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
}

/**
 * Whether, once the fonts the page asked for have loaded, a face of IBM Plex Sans is among them.
 * @param {import('@playwright/test').Page} page
 * @returns {Promise<boolean>}
 */
function plexSansLoaded(page) {
  return page.evaluate(async () => {
    await document.fonts.ready;
    return Array.from(document.fonts).some(
      (face) => face.family.replace(/^["']|["']$/g, '') === 'IBM Plex Sans' && face.status === 'loaded',
    );
  });
}

/**
 * How much wider than the room inside `field` its placeholder is, in pixels: 0 or less when it fits.
 * @param {import('@playwright/test').Locator} field
 * @returns {Promise<number>}
 */
function placeholderOverflow(field) {
  return field.evaluate((input) => {
    if (!(input instanceof HTMLInputElement)) {
      throw new Error('not an input');
    }
    const style = getComputedStyle(input);
    const context = document.createElement('canvas').getContext('2d');
    if (context === null) {
      throw new Error('no canvas to measure with');
    }
    context.font = `${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
    const room = input.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight);
    return context.measureText(input.placeholder).width - room;
  });
}

/**
 * Whether the element at the middle of the inspector is inside it, so that it lies over whatever
 * else is there.
 * @param {import('@playwright/test').Locator} inspector
 * @returns {Promise<boolean>}
 */
function onTop(inspector) {
  return inspector.evaluate((element) => {
    const box = element.getBoundingClientRect();
    const hit = document.elementFromPoint(box.left + box.width / 2, box.top + box.height / 2);
    return hit !== null && element.contains(hit);
  });
}

/**
 * The checks every screen passes at every width and in every theme.
 * @param {import('@playwright/test').Page} page
 * @param {string} baseUrl
 * @param {ScreenCase} screen
 * @param {'light' | 'dark'} theme
 * @returns {Promise<void>}
 */
async function expectSound(page, baseUrl, screen, theme) {
  const answer = await page.goto(`${baseUrl}${screen.address}`);
  expect(answer?.status()).toBe(200);
  expect(await page.evaluate(() => window.matchMedia('(prefers-color-scheme: dark)').matches)).toBe(theme === 'dark');

  const current = page.getByRole('navigation', { name: 'Main' }).locator('a[aria-current="page"]');
  if (screen.nav === null) {
    await expect(current).toHaveCount(0);
  } else {
    await expect(current).toHaveCount(1);
    await expect(current).toHaveAttribute('href', screen.nav);
  }
  expect(await scrollsSideways(page)).toBe(false);
  await expect(page.getByRole('main')).toHaveCount(1);
  await expect(page.getByRole('link', { name: 'Skip to content' })).toHaveAttribute('href', '#main');
  expect(await plexSansLoaded(page)).toBe(true);
}

for (const theme of THEMES) {
  for (const width of WIDTHS) {
    test.describe(`${width} px, ${theme}`, () => {
      test.use({ colorScheme: theme, viewport: { width, height: 900 } });

      for (const screen of SCREENS) {
        test(`${screen.name} at ${width} px in the ${theme} theme`, async ({ page, dibs }) => {
          const seeded = await seedWidgets(dibs);
          await signIn(page, dibs.url, dibs.adminToken);
          await expectSound(page, dibs.url, screen, theme);

          const toggle = page.locator('#rail [data-rail-toggle]');
          await expect(toggle).toHaveCount(1);
          if (width !== PHONE) {
            await expect(toggle).toBeHidden();
          }

          const filter = page.locator('.bar-top form[role="search"] input[name="q"]');
          const narrows = !WITHOUT_FILTER.has(screen.name);
          await expect(filter).toHaveCount(narrows ? 1 : 0);
          if (narrows) {
            expect(await placeholderOverflow(filter)).toBeLessThanOrEqual(0);
            if (width === PHONE) {
              const field = await filter.boundingBox();
              expect(field?.width ?? 0).toBeGreaterThan(PHONE * 0.8);
            }
          }

          if (width === NARROW_SPLIT && screen.inspect !== null) {
            await page.goto(`${dibs.url}${screen.inspect(seeded)}`);
            expect(await scrollsSideways(page)).toBe(false);
            const inspector = page.locator('[data-region="inspector"]');
            await expect(inspector).toHaveAttribute('data-selected', 'true');
            await expect(inspector).toBeVisible();
            await expect(page.locator('[data-region="list"]')).toBeVisible();
            expect(await onTop(inspector)).toBe(true);
            await inspector.getByRole('link', { name: 'Close' }).click();
            await expect(page).toHaveURL(`${dibs.url}${screen.address}`);
            await expect(inspector).toHaveAttribute('data-selected', 'false');
            await expect(inspector).toBeHidden();
          }

          if (width === PHONE) {
            const rail = page.locator('#rail');
            const menu = rail.getByRole('button', { name: 'Menu' });
            const people = rail.getByRole('region', { name: 'People' });
            await expect(menu).toBeVisible();
            await expect(menu).toHaveAttribute('aria-expanded', 'false');
            await expect(people).toBeHidden();
            await menu.click();
            await expect(menu).toHaveAttribute('aria-expanded', 'true');
            await expect(people).toBeVisible();
            await menu.click();
            await expect(menu).toHaveAttribute('aria-expanded', 'false');
            await expect(people).toBeHidden();
          }
        });
      }
    });
  }
}

test('on a window too short for the rail, the rail scrolls and its search box keeps its height', async ({
  page,
  dibs,
}) => {
  await seedWidgets(dibs);
  await page.setViewportSize({ width: 1680, height: 360 });
  await signIn(page, dibs.url, dibs.adminToken);
  await page.goto(`${dibs.url}/ui/areas`);

  const rail = page.locator('#rail');
  expect(await rail.evaluate((element) => element.scrollHeight > element.clientHeight)).toBe(true);
  const search = await rail.locator('form[data-search]').boundingBox();
  expect(search?.height).toBe(34);
});
