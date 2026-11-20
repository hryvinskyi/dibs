// @ts-check
/**
 * The page's notices about its live state: the change notice (`[data-changed]`), which offers a
 * reload, and the session-ended notice (`[data-session-ended]`), which offers a sign-in.
 */

/**
 * @typedef {object} PageNotices
 * @property {(address: string | null) => void} showChanged shows the change notice, its Reload
 *   leading to `address`, or to the address the server rendered there when `address` is `null`
 * @property {() => void} hideChanged
 * @property {() => void} showEnded shows the session-ended notice in place of the change notice
 */

/**
 * Shows or hides every element of the document matching `selector`.
 * @param {Document} doc
 * @param {string} selector
 * @param {boolean} hidden
 * @returns {void}
 */
function setHidden(doc, selector, hidden) {
  doc.querySelectorAll(selector).forEach((element) => {
    if (element instanceof HTMLElement) {
      element.hidden = hidden;
    }
  });
}

/**
 * The notices of `doc`.
 * @param {Document} doc
 * @returns {PageNotices}
 */
export function pageNotices(doc) {
  return {
    showChanged(address) {
      if (address !== null) {
        doc.querySelectorAll('[data-changed] a[href]').forEach((link) => link.setAttribute('href', address));
      }
      setHidden(doc, '[data-changed]', false);
    },
    hideChanged() {
      setHidden(doc, '[data-changed]', true);
    },
    showEnded() {
      setHidden(doc, '[data-changed]', true);
      setHidden(doc, '[data-session-ended]', false);
    },
  };
}
