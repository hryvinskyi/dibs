// @ts-check
/**
 * Content security policy violations a page reports about itself: the browser's
 * `securitypolicyviolation` events, reported to the test from inside every document the page
 * loads, and the console errors that refuse a resource on policy grounds.
 */

/** The name of the binding the documents report through. */
const BINDING = 'reportPolicyViolation';

/**
 * @typedef {object} PolicyWatch
 * @property {() => readonly string[]} violations one line per violation seen so far
 */

/**
 * Starts watching `page`; call before it navigates.
 * @param {import('@playwright/test').Page} page
 * @returns {Promise<PolicyWatch>}
 */
export async function watchPolicy(page) {
  /** @type {string[]} */
  const seen = [];
  await page.exposeBinding(BINDING, (_source, line) => {
    seen.push(String(line));
  });
  await page.addInitScript((binding) => {
    document.addEventListener(
      'securitypolicyviolation',
      (event) => {
        /** @type {unknown} */
        const report = Reflect.get(window, binding);
        if (typeof report !== 'function') {
          return;
        }
        report(`${event.violatedDirective} blocked ${event.blockedURI || 'inline'}`);
      },
      true,
    );
  }, BINDING);
  page.on('console', (message) => {
    if (message.type() === 'error' && /content security policy/i.test(message.text())) {
      seen.push(message.text());
    }
  });
  return { violations: () => seen };
}
