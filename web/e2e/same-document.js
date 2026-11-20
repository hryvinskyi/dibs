// @ts-check
/**
 * Telling that a page was not loaded again: a mark set on the open document is still there only
 * while the browser shows that same document.
 */

import { expect } from './fixtures.js';

/** The attribute the mark is set in. */
const MARK = 'data-e2e-marker';

/**
 * Marks the open document.
 * @param {import('@playwright/test').Page} page
 * @returns {Promise<void>}
 */
export async function markDocument(page) {
  await page.evaluate((mark) => document.body.setAttribute(mark, 'same-document'), MARK);
}

/**
 * Expects the document marked by `markDocument` to be the one shown.
 * @param {import('@playwright/test').Page} page
 * @returns {Promise<void>}
 */
export async function expectSameDocument(page) {
  await expect(page.locator('body')).toHaveAttribute(MARK, 'same-document');
}

/**
 * Marks an element, so that a case can tell when it was replaced by another.
 * @param {import('@playwright/test').Locator} element
 * @returns {Promise<void>}
 */
export async function markElement(element) {
  await element.evaluate((node, mark) => node.setAttribute(mark, 'same-element'), MARK);
}

/**
 * Expects the element marked by `markElement` to have been replaced: what the locator finds now
 * carries no mark.
 * @param {import('@playwright/test').Locator} element
 * @returns {Promise<void>}
 */
export async function expectReplaced(element) {
  await expect(element).not.toHaveAttribute(MARK, 'same-element');
}

/**
 * Expects the element marked by `markElement` to be the one the locator still finds.
 * @param {import('@playwright/test').Locator} element
 * @returns {Promise<void>}
 */
export async function expectKept(element) {
  await expect(element).toHaveAttribute(MARK, 'same-element');
}
