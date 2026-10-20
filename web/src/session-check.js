// @ts-check
/**
 * What the answer of a page's session check, made after its stream was refused, means for the
 * stream: the session ended, the page's cursor is behind what the log still holds and it must read
 * its state again and follow from the newest event the answer names, or the stream may simply be
 * opened again. Pure, so a test reaches it without a page.
 */

/**
 * @typedef {{ kind: 'ended' } | { kind: 'resync', from: number } | { kind: 'retry' }} SessionVerdict
 */

/**
 * @param {unknown} value
 * @returns {value is Record<string, unknown>}
 */
const isRecord = (value) => typeof value === 'object' && value !== null && !Array.isArray(value);

/**
 * The newest event an expired-cursor answer names, at `error.details.latest`; 0, the start of
 * the log, when the body does not say.
 * @param {unknown} body
 * @returns {number}
 */
function latestOf(body) {
  const error = isRecord(body) ? body.error : undefined;
  const details = isRecord(error) ? error.details : undefined;
  const latest = isRecord(details) ? details.latest : undefined;
  return typeof latest === 'number' && Number.isSafeInteger(latest) && latest >= 0 ? latest : 0;
}

/**
 * The verdict on a session check's answer.
 * @param {number} status the HTTP status, 0 when the check failed before an answer
 * @param {unknown} body the parsed JSON body of a `410`, `null` otherwise
 * @returns {SessionVerdict}
 */
export function sessionVerdict(status, body) {
  if (status === 401) {
    return { kind: 'ended' };
  }
  if (status === 410) {
    return { kind: 'resync', from: latestOf(body) };
  }
  return { kind: 'retry' };
}
