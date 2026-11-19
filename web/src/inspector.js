// @ts-check
/**
 * Navigation inside a screen without a page load: a plain click on a link that selects an object
 * of the screen (`a[data-inspect]`) fetches the page at its address and swaps its inspector in,
 * pushes the address to the history, marks the row of that object as current and moves the focus
 * to the inspector's heading. Closing the inspector (its close link, or `Escape`) returns the
 * focus to the row that was selected, or to the list's heading. The browser's Back and Forward
 * swap the inspector of the address they land on. A modified click, another button or another
 * origin is left to the browser; an answer that cannot be shown loads the address as a page.
 */

import { findRegion, swapRegion } from './regions.js';

/** The regions an in-place navigation asks for. */
const INSPECTOR_ONLY = Object.freeze(['inspector']);

/** What a pushed history entry carries, so the page can tell its own entries. */
const HISTORY_STATE = Object.freeze({ dibsInspect: true });

/** What counts as a field, where `Escape` belongs to what the person is typing. */
const FIELD = 'input, textarea, select, [contenteditable]:not([contenteditable="false"])';

/**
 * What the inspector's navigation is given.
 * @typedef {object} InspectorDeps
 * @property {import('./regions.js').FetchRegions} fetchRegions
 * @property {Pick<History, 'pushState'>} history
 * @property {Pick<Location, 'href' | 'assign'>} location
 * @property {(region: Element) => void} onSwap called with each inspector swapped in
 * @property {() => void} onNavigate called when a navigation starts, before its request leaves
 * @property {() => void} showSignedOut called when the session turns out to have ended
 */

/**
 * Whether a click is the primary button with no modifier key, not handled already.
 * @param {Pick<MouseEvent, 'button' | 'ctrlKey' | 'metaKey' | 'shiftKey' | 'altKey' | 'defaultPrevented'>} event
 * @returns {boolean}
 */
export function isPlainClick(event) {
  return (
    event.button === 0 &&
    !event.ctrlKey &&
    !event.metaKey &&
    !event.shiftKey &&
    !event.altKey &&
    !event.defaultPrevented
  );
}

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
 * Whether two absolute addresses are on one origin.
 * @param {string} a
 * @param {string} b
 * @returns {boolean}
 */
function sameOrigin(a, b) {
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

/**
 * Whether two absolute addresses name one page: one origin, one path, one query.
 * @param {string} a
 * @param {string} b
 * @returns {boolean}
 */
function sameAddress(a, b) {
  const first = parsed(a);
  const second = parsed(b);
  return (
    first !== null &&
    second !== null &&
    first.origin === second.origin &&
    first.pathname === second.pathname &&
    first.search === second.search
  );
}

/**
 * Which of the rows' links (absolute addresses, `''` for a row without one) points at the page
 * shown, or -1.
 * @param {readonly string[]} hrefs
 * @param {string} pathname the path of the page shown
 * @returns {number}
 */
export function selectedIndex(hrefs, pathname) {
  return hrefs.findIndex((href) => parsed(href)?.pathname === pathname);
}

/**
 * Which of the inspector's links (absolute addresses) is its close link: the one that goes back to
 * the list the object shown belongs to, whose path is the page's path without its last segment.
 * @param {readonly string[]} hrefs
 * @param {string} pathname the path of the page shown
 * @returns {number}
 */
export function closeLinkIndex(hrefs, pathname) {
  const cut = pathname.lastIndexOf('/');
  if (cut <= 0) {
    return -1;
  }
  const list = pathname.slice(0, cut);
  return hrefs.findIndex((href) => parsed(href)?.pathname === list);
}

/**
 * Whether a key closes the inspector: `Escape`, not handled already, not typed into a field, with
 * no dialog open and an object selected.
 * @param {{ key: string, inField: boolean, dialogOpen: boolean, selected: boolean, defaultPrevented: boolean }} press
 * @returns {boolean}
 */
export function closesOnKey({ key, inField, dialogOpen, selected, defaultPrevented }) {
  return key === 'Escape' && !defaultPrevented && !inField && !dialogOpen && selected;
}

/**
 * The link a row selects its object with: the row itself when it is that link.
 * @param {Element} row
 * @returns {HTMLAnchorElement | null}
 */
function rowLink(row) {
  if (row instanceof HTMLAnchorElement && row.hasAttribute('data-inspect')) {
    return row;
  }
  const link = row.querySelector('a[data-inspect]');
  return link instanceof HTMLAnchorElement ? link : null;
}

/**
 * The rows of the screen's list.
 * @param {Element} app
 * @returns {Element[]}
 */
function listRows(app) {
  const list = findRegion(app, 'list');
  return list === null ? [] : Array.from(list.querySelectorAll('[data-row]'));
}

/**
 * The link of the row marked as current, if any.
 * @param {Element} app
 * @returns {HTMLAnchorElement | null}
 */
function currentRowLink(app) {
  const row = listRows(app).find((candidate) => candidate.getAttribute('aria-current') === 'true');
  return row === undefined ? null : rowLink(row);
}

/**
 * Marks the row whose link points at `pathname` as current; the others lose the mark.
 * @param {Element} app
 * @param {string} pathname
 * @returns {void}
 */
function markSelected(app, pathname) {
  const rows = listRows(app);
  const chosen = selectedIndex(rows.map((row) => rowLink(row)?.href ?? ''), pathname);
  rows.forEach((row, index) => {
    if (index === chosen) {
      row.setAttribute('aria-current', 'true');
    } else {
      row.removeAttribute('aria-current');
    }
  });
}

/**
 * Moves the focus to an element that can hold it.
 * @param {Element | null | undefined} target
 * @returns {boolean} whether it moved
 */
function focusOn(target) {
  if (target instanceof HTMLElement && target.isConnected) {
    target.focus();
    return true;
  }
  return false;
}

/** @type {WeakSet<Element>} */
const mounted = new WeakSet();

/**
 * Makes `data-inspect` links swap the inspector region in place. Mounting the same element again
 * does nothing.
 * @param {Element} app the `.app` element
 * @param {InspectorDeps} deps
 * @returns {() => void} teardown
 */
export function mountInspector(app, deps) {
  if (mounted.has(app)) {
    return () => {};
  }
  mounted.add(app);
  const { fetchRegions, history, location, onSwap, onNavigate, showSignedOut } = deps;
  const doc = app.ownerDocument;
  const controller = new AbortController();
  const { signal } = controller;
  /** @type {AbortController | null} */
  let inflight = null;
  let shown = location.href;

  /**
   * Shows the inspector of the answer: swaps it in, records the address, marks the row and moves
   * the focus.
   * @param {Element} incoming
   * @param {string} href
   * @param {boolean} push
   * @param {string} wasSelected the address of the row selected before, `''` for none
   */
  const show = (incoming, href, push, wasSelected) => {
    const current = findRegion(app, 'inspector');
    if (current === null) {
      location.assign(href);
      return;
    }
    const next = swapRegion(current, incoming);
    if (push) {
      history.pushState(HISTORY_STATE, '', href);
    }
    shown = href;
    markSelected(app, parsed(href)?.pathname ?? '');
    onSwap(next);
    if (next.getAttribute('data-selected') === 'false') {
      const back = listRows(app)
        .map(rowLink)
        .find((link) => link !== null && wasSelected !== '' && samePath(link.href, wasSelected));
      if (!focusOn(back)) {
        focusOn(findRegion(app, 'list')?.querySelector('h1'));
      }
      return;
    }
    focusOn(next.querySelector('[data-inspector-heading]'));
  };

  /**
   * What an answer without regions leads to.
   * @type {{ [K in Exclude<import('./regions.js').RegionsAnswer['kind'], 'ok'>]: (href: string) => void }}
   */
  const unanswered = {
    'signed-out': () => showSignedOut(),
    failed: (href) => location.assign(href),
  };

  /**
   * @param {string} href
   * @param {boolean} push whether the address goes into the history
   */
  const navigate = async (href, push) => {
    if (findRegion(app, 'inspector') === null) {
      location.assign(href);
      return;
    }
    inflight?.abort();
    const mine = new AbortController();
    inflight = mine;
    onNavigate();
    const wasSelected = currentRowLink(app)?.href ?? '';
    /** @type {import('./regions.js').RegionsAnswer} */
    let answer;
    try {
      answer = await fetchRegions(href, INSPECTOR_ONLY, { signal: AbortSignal.any([signal, mine.signal]) });
    } catch (error) {
      if (mine.signal.aborted || signal.aborted) {
        return;
      }
      console.error('dibs: the inspector could not be fetched', error);
      location.assign(href);
      return;
    } finally {
      if (inflight === mine) {
        inflight = null;
      }
    }
    if (mine.signal.aborted || signal.aborted) {
      return;
    }
    if (answer.kind === 'ok') {
      const incoming = answer.regions.get('inspector');
      if (incoming === undefined) {
        location.assign(href);
        return;
      }
      show(incoming, href, push, wasSelected);
      return;
    }
    unanswered[answer.kind](href);
  };

  /** @param {string} href @param {boolean} push */
  const go = (href, push) => {
    navigate(href, push).catch((error) => {
      console.error('dibs: an in-place navigation failed', error);
    });
  };

  app.addEventListener('click', (event) => {
    if (!(event instanceof MouseEvent) || !isPlainClick(event) || !(event.target instanceof Element)) {
      return;
    }
    const link = event.target.closest('a[data-inspect]');
    if (!(link instanceof HTMLAnchorElement) || !app.contains(link)) {
      return;
    }
    if (link.hasAttribute('download') || (link.target !== '' && link.target !== '_self')) {
      return;
    }
    if (!sameOrigin(link.href, location.href)) {
      return;
    }
    event.preventDefault();
    go(link.href, true);
  }, { signal });

  doc.addEventListener('keydown', (event) => {
    const inspector = findRegion(app, 'inspector');
    const press = {
      key: event.key,
      inField: event.target instanceof Element && event.target.closest(FIELD) !== null,
      dialogOpen: doc.querySelector('dialog[open]') !== null,
      selected: inspector?.getAttribute('data-selected') === 'true',
      defaultPrevented: event.defaultPrevented,
    };
    if (inspector === null || !closesOnKey(press)) {
      return;
    }
    const links = Array.from(inspector.querySelectorAll('a[data-inspect]')).filter(
      (link) => link instanceof HTMLAnchorElement,
    );
    const close = links[closeLinkIndex(links.map((link) => link.href), parsed(location.href)?.pathname ?? '')];
    if (close === undefined) {
      return;
    }
    event.preventDefault();
    go(close.href, true);
  }, { signal });

  doc.defaultView?.addEventListener('popstate', () => {
    const href = location.href;
    if (sameAddress(href, shown)) {
      return;
    }
    go(href, false);
  }, { signal });

  return () => {
    controller.abort();
    inflight?.abort();
    mounted.delete(app);
  };
}
