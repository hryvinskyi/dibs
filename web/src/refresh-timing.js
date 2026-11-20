// @ts-check
/**
 * What a live page does when what it shows is due to be fetched again: a pure decision over what
 * the page reports, so a test reaches it without a page. Nothing is fetched once the session has
 * ended, nor while the tab is hidden.
 */

/**
 * `fetch` now; `owe` one fetch for when the tab shows again; `stop`, because the session has ended.
 * @typedef {'fetch' | 'owe' | 'stop'} DueAction
 */

/**
 * @typedef {object} PageState
 * @property {boolean} ended whether the page's session has ended
 * @property {boolean} hidden whether the tab is hidden
 */

/**
 * What to do when a refresh comes due.
 * @param {PageState} state
 * @returns {DueAction}
 */
export function whenDue({ ended, hidden }) {
  if (ended) {
    return 'stop';
  }
  return hidden ? 'owe' : 'fetch';
}
