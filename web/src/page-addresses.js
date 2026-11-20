// @ts-check
/**
 * Comparing the absolute addresses of pages: the path of one, and whether two are on one origin or
 * name one path. The query and the fragment never matter, and a value that is not an absolute
 * address matches nothing.
 */

/**
 * An absolute address parsed; `null` when it is not one.
 * @param {string} address
 * @returns {URL | null}
 */
function parsed(address) {
  try {
    return new URL(address);
  } catch {
    return null;
  }
}

/**
 * The path of an absolute address; `null` when it is not one.
 * @param {string} address
 * @returns {string | null}
 */
export function pathOf(address) {
  return parsed(address)?.pathname ?? null;
}

/**
 * Whether two absolute addresses are on one origin.
 * @param {string} a
 * @param {string} b
 * @returns {boolean}
 */
export function sameOrigin(a, b) {
  const first = parsed(a);
  const second = parsed(b);
  return first !== null && second !== null && first.origin === second.origin;
}

/**
 * Whether two absolute addresses are on one origin with one path; the query and the fragment are
 * ignored.
 * @param {string} a
 * @param {string} b
 * @returns {boolean}
 */
export function samePath(a, b) {
  const first = parsed(a);
  const second = parsed(b);
  return first !== null && second !== null && first.origin === second.origin && first.pathname === second.pathname;
}
