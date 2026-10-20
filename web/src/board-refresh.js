// @ts-check
/**
 * What the board does when its columns are due to be rendered again, and what it does with the
 * answer: pure decisions over what the page reports, so a test reaches them without a page.
 *
 * The columns are never replaced while the person is using a form in them, and never with an
 * answer that is not the board's own fragment: a sign-in page, an error page or a proxy's answer
 * is never swapped in.
 */

/** The value of the header that marks an answer as the board's columns. */
export const BOARD_FRAGMENT = 'board';

/**
 * `fetch` the columns now; `owe` one fetch for when the tab shows again; `hold` and offer a
 * reload instead, because the person is using a form; `stop`, because the session has ended.
 * @typedef {'fetch' | 'owe' | 'hold' | 'stop'} DueAction
 */

/**
 * @typedef {object} PageState
 * @property {boolean} ended whether the page's session has ended
 * @property {boolean} hidden whether the tab is hidden
 * @property {boolean} typing whether a form inside the columns has focus or holds input
 */

/**
 * What to do when a refresh comes due.
 * @param {PageState} state
 * @returns {DueAction}
 */
export function whenDue({ ended, hidden, typing }) {
  if (ended) {
    return 'stop';
  }
  if (hidden) {
    return 'owe';
  }
  return typing ? 'hold' : 'fetch';
}

/**
 * The parts of an answer the decision reads.
 * @typedef {object} Answer
 * @property {number} status
 * @property {string} contentType the `Content-Type` header, empty when absent
 * @property {string} fragment the `X-Dibs-Fragment` header, empty when absent
 */

/**
 * `swap` the columns in; `hold` and offer a reload, because the person started using a form while
 * the answer was on its way; `ended`, because the session has ended; `failed` for any other answer.
 * @typedef {'swap' | 'hold' | 'ended' | 'failed'} AnswerAction
 */

/**
 * Whether a `Content-Type` header names HTML.
 * @param {string} contentType
 * @returns {boolean}
 */
function isHtml(contentType) {
  const [type = ''] = contentType.split(';');
  return type.trim().toLowerCase() === 'text/html';
}

/**
 * What to do with an answer to the columns' request.
 * @param {Answer} answer
 * @param {boolean} typing whether a form inside the columns has focus or holds input now
 * @returns {AnswerAction}
 */
export function onAnswer(answer, typing) {
  if (answer.status === 401) {
    return 'ended';
  }
  const ours = answer.status === 200 && isHtml(answer.contentType) && answer.fragment === BOARD_FRAGMENT;
  if (!ours) {
    return 'failed';
  }
  return typing ? 'hold' : 'swap';
}
