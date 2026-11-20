// @ts-check
/**
 * The anchor a message of a conversation carries as its element's `id` (`m-{id}`), and the message
 * an anchor names.
 */

/**
 * The anchor of a message.
 * @param {number} message
 * @returns {string}
 */
export function anchorOf(message) {
  return `m-${message}`;
}

/**
 * The message an element's `id` anchors; `null` for any other `id`.
 * @param {string} anchor
 * @returns {number | null}
 */
export function messageOfAnchor(anchor) {
  const match = /^m-(\d+)$/.exec(anchor);
  const id = match === null ? Number.NaN : Number(match[1]);
  return Number.isSafeInteger(id) ? id : null;
}
