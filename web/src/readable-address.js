// @ts-check
/**
 * The address a page's scripts read the page from again, and how they load it again from scratch.
 * It is the address the page shows, except on a page answering a form while it still shows the
 * address the form posted to: only a POST answers that address, so a GET of it, or a reload that
 * posts the form again, is never made; the page is read from the screen's address the server
 * rendered for it instead, the one its change notice's Reload carries.
 */

import { answerStillWanted } from './answer-address.js';
import { asksItsAddress } from './change-following.js';
import { parseScreenConfig } from './screen-config.js';

/**
 * Where a page stands: whether it answers a form, the address it shows now, the one it was loaded
 * at, and the screen's address the server rendered for it (all absolute).
 * @typedef {object} PageAt
 * @property {boolean} answersForm
 * @property {string} shown
 * @property {string} loadedAt
 * @property {string} rendered
 */

/**
 * @typedef {object} PageAddress
 * @property {() => string} readable the address the page is read from again now
 * @property {() => void} reload loads the page again from scratch, from that address
 */

/**
 * The address the page is read from again: the one shown, unless the page answers a form and still
 * shows the address the form posted to, when it is the screen's address the server rendered.
 * @param {PageAt} page
 * @returns {string}
 */
export function readableAddress({ answersForm, shown, loadedAt, rendered }) {
  return answersForm && answerStillWanted(shown, loadedAt) ? rendered : shown;
}

/**
 * Where the page goes to be loaded again from scratch: `null` when the address shown is the
 * readable one, which is then reloaded; otherwise the readable one, which is visited.
 * @param {string} readable
 * @param {string} shown
 * @returns {string | null}
 */
export function reloadTarget(readable, shown) {
  return answerStillWanted(readable, shown) ? null : readable;
}

/**
 * Whether the screen's frame says the page answers a form: it follows no change. A frame whose
 * configuration cannot be read is taken as one that does not; the screen reports it.
 * @param {import('./view-config.js').DataAttributes} data the `.app` element's attributes
 * @returns {boolean}
 */
function answersForm(data) {
  try {
    return !asksItsAddress(parseScreenConfig(data).kinds);
  } catch {
    return false;
  }
}

/**
 * The readable address of the page `app` is the frame of, as `location` shows it from now on.
 * @param {HTMLElement} app the `.app` element
 * @param {Pick<Location, 'href' | 'reload' | 'assign'>} location
 * @returns {PageAddress}
 */
export function pageAddress(app, location) {
  const loadedAt = location.href;
  const reloadLink = app.ownerDocument.querySelector('[data-changed] a[href]');
  const rendered = reloadLink instanceof HTMLAnchorElement ? reloadLink.href : loadedAt;
  const form = answersForm(app.dataset);
  const readable = () => readableAddress({ answersForm: form, shown: location.href, loadedAt, rendered });
  return {
    readable,
    reload: () => {
      const target = reloadTarget(readable(), location.href);
      if (target === null) {
        location.reload();
      } else {
        location.assign(target);
      }
    },
  };
}
