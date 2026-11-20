// @ts-check
/** Signing a member in through the login page, as every case starts. */

/**
 * Signs in on the login page with `token` and waits for Now, where signing in lands, to have
 * loaded, its scripts with it.
 * @param {import('@playwright/test').Page} page
 * @param {string} baseUrl
 * @param {string} token
 * @returns {Promise<void>}
 */
export async function signIn(page, baseUrl, token) {
  await page.goto(`${baseUrl}/ui/login`);
  await page.getByLabel('Paste your token').fill(token);
  await page.getByRole('button', { name: 'Sign in' }).click();
  await page.waitForURL(`${baseUrl}/ui/`, { waitUntil: 'load' });
}
