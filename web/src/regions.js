// @ts-check
/**
 * The named regions of a page (`[data-region]`): fetching a page of this service and reading the
 * regions out of its answer, and swapping a region of the page shown for the one of the same name
 * in an answer. Nothing else is read out of an answer, and an answer is read only when it is the
 * page of a live session: a sign-in page, an error page or a proxy's answer never reaches the page.
 *
 * What the scripts set on rendered markup that the server does not render survives a swap: the
 * rail's collapsed state and the open `<details>` the old region and the new one share by `id`.
 */

/** How long a page may take to answer, in milliseconds, when the caller does not say. */
const DEFAULT_TIMEOUT_MS = 5000;

/** The attributes a script sets on a region element, carried from the old region to the new one. */
const CARRIED_ATTRIBUTES = Object.freeze(['data-collapsible', 'data-open']);

/**
 * A node of a parsed answer, as far as the regions are read from it.
 * @typedef {{ getAttribute(name: string): string | null }} RegionNode
 */

/**
 * A parsed answer, as far as the regions are read from it.
 * @template {RegionNode} E
 * @typedef {{ querySelectorAll(selectors: string): ArrayLike<E> }} ParsedAnswer
 */

/**
 * The regions of an answer by name; the session ended; or the answer cannot be shown.
 * @template {RegionNode} E
 * @typedef {{ kind: 'ok', regions: Map<string, E> } | { kind: 'signed-out' } | { kind: 'failed' }} RegionsAnswerOf
 */

/** @typedef {RegionsAnswerOf<Element>} RegionsAnswer */

/**
 * What fetching regions needs besides the address and the names.
 * @typedef {object} RequestOptions
 * @property {AbortSignal} [signal] the caller's: aborting it rejects with its reason
 * @property {number} [timeoutMs] the deadline, 5000 when absent
 * @property {string} [loginPath] the sign-in page's path: a redirect that lands there means the
 *   session ended
 * @property {typeof fetch} [fetchImpl]
 */

/**
 * Fetching the regions of a page, as the screens' scripts are given it.
 * @typedef {(url: string, names: readonly string[], options?: { signal?: AbortSignal }) => Promise<RegionsAnswer>}
 *   FetchRegions
 */

/**
 * `read` the answer's regions; `signed-out`, because the session ended; `failed` for an answer
 * that is not a page of this service.
 * @typedef {'read' | 'signed-out' | 'failed'} AnswerVerdict
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
 * The path of an absolute address; `null` when it is not one.
 * @param {string} address
 * @returns {string | null}
 */
function pathOf(address) {
  try {
    return new URL(address).pathname;
  } catch {
    return null;
  }
}

/**
 * The verdict on an answer before its body is read: a `401`, or a redirect that lands on the
 * sign-in page, means the session ended; a `200` or a `404` in HTML is a page whose regions may be
 * read; anything else fails.
 * @param {{ status: number, redirected: boolean, finalPath: string | null, contentType: string }} answer
 * @param {string | undefined} loginPath
 * @returns {AnswerVerdict}
 */
export function answerVerdict({ status, redirected, finalPath, contentType }, loginPath) {
  if (status === 401) {
    return 'signed-out';
  }
  if (redirected && loginPath !== undefined && finalPath === loginPath) {
    return 'signed-out';
  }
  return (status === 200 || status === 404) && isHtml(contentType) ? 'read' : 'failed';
}

/**
 * The regions of a parsed answer by name, the first of each name; `null` when one is missing.
 * @template {RegionNode} E
 * @param {ParsedAnswer<E>} parsed
 * @param {readonly string[]} names
 * @returns {Map<string, E> | null}
 */
function pickRegions(parsed, names) {
  /** @type {Map<string, E>} */
  const found = new Map();
  for (const node of Array.from(parsed.querySelectorAll('[data-region]'))) {
    const name = node.getAttribute('data-region');
    if (name !== null && names.includes(name) && !found.has(name)) {
      found.set(name, node);
    }
  }
  return names.every((name) => found.has(name)) ? found : null;
}

/**
 * Fetches a page of this service and returns the named regions of the answer, parsed by `parse`.
 * @template {RegionNode} E
 * @param {string} url same-origin address
 * @param {readonly string[]} names the data-region names wanted
 * @param {(html: string) => ParsedAnswer<E>} parse
 * @param {RequestOptions} [options]
 * @returns {Promise<RegionsAnswerOf<E>>}
 */
export async function requestRegions(url, names, parse, options = {}) {
  const { signal: caller, timeoutMs = DEFAULT_TIMEOUT_MS, loginPath, fetchImpl = fetch } = options;
  const deadline = AbortSignal.timeout(timeoutMs);
  try {
    const response = await fetchImpl(url, {
      method: 'GET',
      credentials: 'same-origin',
      redirect: 'follow',
      cache: 'no-store',
      headers: { Accept: 'text/html' },
      signal: caller === undefined ? deadline : AbortSignal.any([caller, deadline]),
    });
    const verdict = answerVerdict(
      {
        status: response.status,
        redirected: response.redirected,
        finalPath: pathOf(response.url),
        contentType: response.headers.get('Content-Type') ?? '',
      },
      loginPath,
    );
    if (verdict !== 'read') {
      caller?.throwIfAborted();
      return { kind: verdict };
    }
    const html = await response.text();
    caller?.throwIfAborted();
    const regions = pickRegions(parse(html), names);
    if (regions === null) {
      console.error('dibs: a page answered without a region it was asked for', url);
      return { kind: 'failed' };
    }
    return { kind: 'ok', regions };
  } catch (error) {
    if (caller?.aborted) {
      throw error;
    }
    console.error('dibs: a page could not be read', url, error);
    return { kind: 'failed' };
  }
}

/**
 * Parses an answer's text as an HTML document, inert: its scripts never run.
 * @param {string} html
 * @returns {Document}
 */
function parseHtml(html) {
  return new DOMParser().parseFromString(html, 'text/html');
}

/**
 * Fetches a page of this service and returns the named regions of the answer.
 * @param {string} url same-origin address
 * @param {readonly string[]} names the data-region names wanted
 * @param {RequestOptions & { parse?: (html: string) => Document }} [options]
 * @returns {Promise<RegionsAnswer>}
 */
export function fetchRegions(url, names, options = {}) {
  /** @type {(html: string) => ParsedAnswer<Element>} */
  const parse = options.parse ?? parseHtml;
  return requestRegions(url, names, parse, options);
}

/**
 * The region of a name inside `scope`, matched by the attribute's value, never by a selector built
 * from it.
 * @param {ParentNode} scope
 * @param {string} name
 * @returns {Element | null}
 */
export function findRegion(scope, name) {
  return Array.from(scope.querySelectorAll('[data-region]')).find((node) => node.getAttribute('data-region') === name)
    ?? null;
}

/**
 * What a region shows that the scripts set: the values of its element's attributes (`null` for
 * one it lacks) and the ids of its open `<details>`.
 * @typedef {{ attrs: Record<string, string | null>, openDetails: string[] }} RegionState
 */

/**
 * What a swap carries from the old region to the new one: the attributes the scripts set on a
 * region element, each with the old region's value or `null` for absent, and the ids of the open
 * `<details>`, once each.
 * @param {RegionState} old
 * @returns {RegionState}
 */
export function carriedState(old) {
  /** @type {Record<string, string | null>} */
  const attrs = {};
  for (const name of CARRIED_ATTRIBUTES) {
    attrs[name] = old.attrs[name] ?? null;
  }
  return { attrs, openDetails: [...new Set(old.openDetails.filter((id) => id !== ''))] };
}

/**
 * Reads what a region shows that the scripts set.
 * @param {Element} region
 * @returns {RegionState}
 */
function readState(region) {
  /** @type {Record<string, string | null>} */
  const attrs = {};
  for (const name of region.getAttributeNames()) {
    attrs[name] = region.getAttribute(name);
  }
  const openDetails = Array.from(region.querySelectorAll('details[id]'))
    .filter((details) => details instanceof HTMLDetailsElement && details.open)
    .map((details) => details.id);
  return { attrs, openDetails };
}

/**
 * Applies a carried state to a region.
 * @param {Element} region
 * @param {RegionState} state
 * @returns {void}
 */
function applyState(region, state) {
  for (const [name, value] of Object.entries(state.attrs)) {
    if (value === null) {
      region.removeAttribute(name);
    } else {
      region.setAttribute(name, value);
    }
  }
  const open = new Set(state.openDetails);
  region.querySelectorAll('details[id]').forEach((details) => {
    if (details instanceof HTMLDetailsElement && open.has(details.id)) {
      details.open = true;
    }
  });
}

/**
 * Replaces `current` with a copy of `incoming` adopted into `current`'s document, carrying over
 * what the scripts set on the rendered markup; returns the new element.
 * @param {Element} current
 * @param {Element} incoming
 * @returns {Element}
 */
export function swapRegion(current, incoming) {
  const next = current.ownerDocument.importNode(incoming, true);
  applyState(next, carriedState(readState(current)));
  current.replaceWith(next);
  return next;
}
