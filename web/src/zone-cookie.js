// @ts-check
/**
 * The cookie that tells the server the viewer's UTC offset, so that a page counts "today" from the
 * viewer's own midnight: whole minutes east of UTC, written only when it changed.
 */

/** The cookie's name. */
export const ZONE_COOKIE = 'dibs_tz';

/** How long the browser keeps it, in seconds: a year. */
const ONE_YEAR_SECONDS = 31_536_000;

/**
 * The value a `document.cookie` text holds under a name, if it holds one.
 * @param {string} cookies `name=value` pairs separated by `;`
 * @param {string} name
 * @returns {string | null}
 */
export function cookieValue(cookies, name) {
  for (const pair of cookies.split(';')) {
    const at = pair.indexOf('=');
    if (at !== -1 && pair.slice(0, at).trim() === name) {
      return pair.slice(at + 1).trim();
    }
  }
  return null;
}

/**
 * The cookie to write for a viewer `offsetMinutes` east of UTC, or `null` when `cookies` already
 * holds that offset.
 * @param {string} cookies the current `document.cookie`
 * @param {number} offsetMinutes minutes east of UTC, as `-Date#getTimezoneOffset()` gives them
 * @returns {string | null}
 */
export function zoneCookie(cookies, offsetMinutes) {
  const value = String(Math.trunc(offsetMinutes) || 0);
  if (cookieValue(cookies, ZONE_COOKIE) === value) {
    return null;
  }
  return `${ZONE_COOKIE}=${value}; Path=/ui; Max-Age=${ONE_YEAR_SECONDS}; SameSite=Strict; Secure`;
}
