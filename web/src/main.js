// @ts-check
/**
 * The entry point of every page: records the viewer's offset and localizes the page's times; on a
 * page drawn in the frame (its `.app` element), it also makes the rail a menu on narrow screens,
 * serves the copy buttons, keeps the screen's regions current from the page's one event stream,
 * swaps the inspector in place, opens the search palette and keeps each conversation live on that
 * same stream. Each region swapped in is localized, its copy buttons shown, the rail's menu state
 * applied, and the conversation in it mounted, while the one of the region it replaced is torn down.
 * Whatever finds the session ended shows the page's session-ended notice.
 */

import { mountCopy } from './copy.js';
import { mountInspector } from './inspector.js';
import { localizeTimes, rememberZone } from './local-times.js';
import { mountPalette } from './palette.js';
import { mountRailMenu } from './rail-menu.js';
import { fetchRegions } from './regions.js';
import { pageAddress } from './readable-address.js';
import { mountScreen } from './screen-view.js';
import { catchingUp } from './stream-catch-up.js';
import { mount as mountThread } from './thread-view.js';
import { isLocalPath } from './view-config.js';

/**
 * Fetching the regions of a page of this service, telling an answer that lands on the sign-in page
 * at `loginPath` as an ended session.
 * @param {string | undefined} loginPath
 * @returns {import('./regions.js').FetchRegions}
 */
function regionsOfPages(loginPath) {
  const login = loginPath !== undefined && isLocalPath(loginPath) ? loginPath : undefined;
  return (url, names, options = {}) => fetchRegions(url, names, { ...options, loginPath: login });
}

/** How many of the stream's newest events a conversation mounted later can catch up on. */
const CATCH_UP_EVENTS = 512;

/** The system's clock, given to the modules that write times against the current one. */
const clock = Object.freeze({ now: () => new Date() });

/**
 * Shows every session-ended notice of the page.
 * @param {Document} doc
 * @returns {void}
 */
function showSignedOut(doc) {
  doc.querySelectorAll('[data-session-ended]').forEach((element) => {
    if (element instanceof HTMLElement) {
      element.hidden = false;
    }
  });
}

/**
 * Wires the live behaviour of a page drawn in the frame.
 * @param {HTMLElement} app the `.app` element
 * @returns {void}
 */
function startScreen(app) {
  const doc = app.ownerDocument;
  const pages = regionsOfPages(app.dataset.loginUrl);
  const signedOut = () => showSignedOut(doc);
  const address = pageAddress(app, window.location);
  const railMenu = mountRailMenu(app);
  const copy = mountCopy(doc.body, {
    clipboard: window.navigator.clipboard,
    location: window.location,
    toast: doc.querySelector('[data-toast]'),
  });
  /** The conversations mounted, by root, with their teardowns. @type {Map<HTMLElement, () => void>} */
  const conversations = new Map();
  /** @type {import('./stream-catch-up.js').SubscribeAfter} */
  let subscribe = () => () => {};

  /**
   * Tears down the conversations no longer on the page and mounts those inside `scope`.
   * @param {ParentNode} scope
   */
  const mountConversations = (scope) => {
    for (const [root, teardown] of conversations) {
      if (!root.isConnected) {
        teardown();
        conversations.delete(root);
      }
    }
    scope.querySelectorAll('[data-thread-view]').forEach((root) => {
      if (root instanceof HTMLElement && !conversations.has(root)) {
        conversations.set(
          root,
          mountThread(root, { clock, subscribe, fetchRegions: pages, address }),
        );
      }
    });
  };

  /** @param {Element} region */
  const onSwap = (region) => {
    localizeTimes(region, clock);
    copy.reveal(region);
    if (region.getAttribute('data-region') === 'rail') {
      railMenu.sync(region);
    }
    mountConversations(region);
  };

  const screen = mountScreen(app, { fetchRegions: pages, location: window.location, onSwap });
  subscribe = catchingUp(screen.subscribe, CATCH_UP_EVENTS);
  mountInspector(app, {
    address,
    fetchRegions: pages,
    history: window.history,
    location: window.location,
    onSwap,
    onNavigate: screen.onNavigate,
    showSignedOut: signedOut,
  });
  mountPalette(doc, app, {
    fetchRegions: pages,
    location: window.location,
    open: (url, target, features) => window.open(url, target, features),
    onSignedOut: signedOut,
  });
  mountConversations(doc);
}

function start() {
  rememberZone(document, clock);
  localizeTimes(document, clock);
  const app = document.querySelector('.app');
  if (app instanceof HTMLElement) {
    startScreen(app);
  }
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', start, { once: true });
} else {
  start();
}
