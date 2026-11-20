// @ts-check
/**
 * Navigation inside a screen without a page load: a plain click on a link that selects an object
 * of the screen (`a[data-inspect]`) fetches the page at its address, pushes the address to the
 * history, and shows what the answer holds: its inspector swapped in with the focus on its heading,
 * and the screen's other regions brought up to date from the same answer, so that what opening the
 * object changed shows at once (`opening-regions.js` decides which); the row of that object is
 * marked as current. Closing the inspector (its close link, or `Escape`) returns the focus to the
 * row that was selected, or to the list's heading. The browser's Back and Forward show the address
 * they land on the same way, from the address the page is read from: a page answering a form shows
 * the address the form posted to, which only a POST answers, so Back there shows the screen's own
 * address. A modified click, another button or another origin is left to the
 * browser; an answer that cannot be shown loads the address as a page.
 */

import { closesOnKey, isPlainClick } from './inspect-input.js';
import { closeLinkIndex } from './list-selection.js';
import { currentRowLink, listRows, markSelected, rowLink } from './list-rows.js';
import { OPENED_REGION, openingAction, regionsToAsk } from './opening-regions.js';
import { answerStillWanted } from './answer-address.js';
import { hasEdits } from './form-use.js';
import { swapKeepingPlace } from './region-swap.js';
import { pathOf, sameOrigin, samePath } from './page-addresses.js';
import { findRegion, swapRegion } from './regions.js';

/** What a pushed history entry carries, so the page can tell its own entries. */
const HISTORY_STATE = Object.freeze({ dibsInspect: true });

/** What counts as a field, where `Escape` belongs to what the person is typing. */
const FIELD = 'input, textarea, select, [contenteditable]:not([contenteditable="false"])';

/**
 * What the inspector's navigation is given.
 * @typedef {object} InspectorDeps
 * @property {Pick<import('./readable-address.js').PageAddress, 'readable'>} address the address the
 *   page is read from again, which a page answering a form does not show
 * @property {import('./regions.js').FetchRegions} fetchRegions
 * @property {Pick<History, 'pushState'>} history
 * @property {Pick<Location, 'href' | 'assign'>} location
 * @property {(region: Element) => void} onSwap called with each region swapped in
 * @property {() => void} onNavigate called when a navigation starts, before its request leaves
 * @property {() => void} showSignedOut called when the session turns out to have ended
 */

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

/**
 * What each opening action does with a region: the region swapped in, or `null` when it is kept.
 * @type {{
 *   [A in import('./opening-regions.js').OpeningAction]: (current: Element, incoming: Element) => Element | null
 * }}
 */
const OPENING = {
  open: swapRegion,
  refresh: swapKeepingPlace,
  keep: () => null,
};

/** @type {WeakSet<Element>} */
const mounted = new WeakSet();

/**
 * Makes `data-inspect` links show their object in place. Mounting the same element again does
 * nothing.
 * @param {Element} app the `.app` element
 * @param {InspectorDeps} deps
 * @returns {() => void} teardown
 */
export function mountInspector(app, deps) {
  if (mounted.has(app)) {
    return () => {};
  }
  mounted.add(app);
  const { address, fetchRegions, history, location, onSwap, onNavigate, showSignedOut } = deps;
  const doc = app.ownerDocument;
  const controller = new AbortController();
  const { signal } = controller;
  /** @type {AbortController | null} */
  let inflight = null;
  let shown = address.readable();

  /**
   * Shows the answer: swaps its regions in, records the address, marks the row and moves the
   * focus.
   * @param {Map<string, Element>} regions
   * @param {readonly string[]} names the regions asked for, in the order they are swapped in
   * @param {string} href
   * @param {boolean} push
   * @param {string} wasSelected the address of the row selected before, `''` for none
   */
  const show = (regions, names, href, push, wasSelected) => {
    if (findRegion(app, OPENED_REGION) === null) {
      location.assign(href);
      return;
    }
    /** @type {Element[]} */
    const swapped = [];
    for (const name of names) {
      const current = findRegion(app, name);
      const incoming = regions.get(name);
      if (current === null || incoming === undefined) {
        continue;
      }
      const next = OPENING[openingAction(name, hasEdits(current))](current, incoming);
      if (next !== null) {
        swapped.push(next);
      }
    }
    if (push) {
      history.pushState(HISTORY_STATE, '', href);
    }
    shown = href;
    markSelected(app, href);
    swapped.forEach(onSwap);
    const opened = findRegion(app, OPENED_REGION);
    if (opened === null) {
      return;
    }
    if (opened.getAttribute('data-selected') === 'false') {
      const back = listRows(app)
        .map(rowLink)
        .find((link) => link !== null && wasSelected !== '' && samePath(link.href, wasSelected));
      if (!focusOn(back)) {
        focusOn(findRegion(app, 'list')?.querySelector('h1'));
      }
      return;
    }
    focusOn(opened.querySelector('[data-inspector-heading]'));
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
    if (findRegion(app, OPENED_REGION) === null) {
      location.assign(href);
      return;
    }
    inflight?.abort();
    const mine = new AbortController();
    inflight = mine;
    onNavigate();
    const wasSelected = currentRowLink(app)?.href ?? '';
    const names = regionsToAsk(
      Array.from(app.querySelectorAll('[data-region]'), (region) => region.getAttribute('data-region') ?? ''),
    );
    /** @type {import('./regions.js').RegionsAnswer} */
    let answer;
    try {
      answer = await fetchRegions(href, names, { signal: AbortSignal.any([signal, mine.signal]) });
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
      if (!answer.regions.has(OPENED_REGION)) {
        location.assign(href);
        return;
      }
      show(answer.regions, names, href, push, wasSelected);
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
    const inspector = findRegion(app, OPENED_REGION);
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
    const close = links[closeLinkIndex(links.map((link) => link.href), pathOf(address.readable()) ?? '')];
    if (close === undefined) {
      return;
    }
    event.preventDefault();
    go(close.href, true);
  }, { signal });

  doc.defaultView?.addEventListener('popstate', () => {
    const href = address.readable();
    if (answerStillWanted(href, shown)) {
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
