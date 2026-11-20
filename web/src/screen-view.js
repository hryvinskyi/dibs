// @ts-check
/**
 * A live screen: while events of the kinds the screen names arrive, the page at the address shown
 * is fetched again and its regions (the rail, the list, the inspector) are swapped in, once per
 * burst of events, one fetch at a time and never while the tab is hidden; a refresh that came due
 * while it was hidden runs once when it shows again. The screen follows the page's one event
 * stream and hands every event, state and resync to its subscribers too.
 *
 * A region its own script keeps current is left as it is. A region the person is using a form in
 * is held, and the page's change notice offers a reload of the address shown; a refresh that
 * swaps every region it asked for hides the notice again, and one is asked for as soon as the
 * person leaves a held form without a change in it. A swapped region keeps its scroll
 * position, and the focus that was inside it goes back to the same element. An answer for an
 * address the page no longer shows is dropped, and an in-place navigation aborts the refresh in
 * flight. When the session is gone, the page's session-ended notice shows and nothing is fetched
 * any more.
 */

import { whenDue } from './refresh-timing.js';
import { coalesce } from './coalesce.js';
import { ContractError, isChange } from './events.js';
import { follow } from './live-stream.js';
import { findRegion, swapRegion } from './regions.js';
import { isLocalPath } from './view-config.js';

/** How long the events of one burst are gathered before one refresh, in milliseconds. */
const BURST_MS = 500;

/** The regions a refresh asks for, those the page has. */
const SCREEN_REGIONS = Object.freeze(['rail', 'list', 'inspector']);

/**
 * The callbacks of `live-stream.js`'s `follow`, each optional.
 * @typedef {object} StreamListener
 * @property {(event: import('./events.js').StreamEvent) => void} [onEvent]
 * @property {(state: import('./live-stream.js').StreamState) => void} [onState]
 * @property {() => void} [onResync]
 */

/**
 * What a live screen is given.
 * @typedef {object} ScreenDeps
 * @property {import('./regions.js').FetchRegions} fetchRegions
 * @property {Pick<Location, 'href'>} location
 * @property {(region: Element) => void} onSwap called with each region swapped in
 */

/**
 * @typedef {object} Screen
 * @property {() => void} teardown
 * @property {() => void} onNavigate drops the refresh in flight; called when the page navigates in
 *   place
 * @property {(listener: StreamListener) => () => void} subscribe hands the stream to `listener`
 *   until the returned function is called
 */

/**
 * The screen's stream, and the kinds of change it refreshes on (`null`: every change).
 * @typedef {{ stream: import('./view-config.js').StreamConfig, kinds: ReadonlySet<string> | null }} ScreenConfig
 */

/**
 * `swap` the region in; `skip` it, because its own script keeps it current; `hold` it, because
 * the person is using it.
 * @typedef {'swap' | 'skip' | 'hold'} RegionAction
 */

/**
 * What to do with a region when a refresh answers.
 * @param {{ ownLive: boolean, focusInside: boolean, edited: boolean }} region `ownLive`: it carries
 *   `data-live="own"`; `focusInside`: a form control in it has the focus; `edited`: a control in it
 *   holds a value other than its default
 * @returns {RegionAction}
 */
export function regionAction({ ownLive, focusInside, edited }) {
  if (ownLive) {
    return 'skip';
  }
  return focusInside || edited ? 'hold' : 'swap';
}

/**
 * Whether an event asks the screen for a refresh: a change of one of its kinds, or any event when
 * the screen lists none.
 * @param {import('./events.js').StreamEvent} event
 * @param {ReadonlySet<string> | null} kinds
 * @returns {boolean}
 */
export function isRelevant(event, kinds) {
  return kinds === null || isChange(event, kinds);
}

/**
 * Whether an answer fetched for one address may still be shown at another: one origin, path and
 * query; the fragment is ignored.
 * @param {string} fetchedFor
 * @param {string} current
 * @returns {boolean}
 */
export function answerStillWanted(fetchedFor, current) {
  try {
    const was = new URL(fetchedFor);
    const now = new URL(current);
    return was.origin === now.origin && was.pathname === now.pathname && was.search === now.search;
  } catch {
    return false;
  }
}

/**
 * Where the focus goes back to after a swap: the candidate with the id of the element that had it,
 * or else the one with its `href`; -1 when none has. An empty id or `href` matches nothing.
 * @param {{ id: string, href: string }} was
 * @param {readonly { id: string, href: string }[]} candidates
 * @returns {number}
 */
export function focusIndex(was, candidates) {
  const byId = was.id === '' ? -1 : candidates.findIndex((candidate) => candidate.id === was.id);
  if (byId !== -1 || was.href === '') {
    return byId;
  }
  return candidates.findIndex((candidate) => candidate.href === was.href);
}

/**
 * A path of this service an attribute names.
 * @param {import('./view-config.js').DataAttributes} data
 * @param {string} name
 * @returns {string}
 */
function localPath(data, name) {
  const value = data[name] ?? '';
  if (!isLocalPath(value)) {
    throw new ContractError(`dataset.${name}`, 'a path of this service');
  }
  return value;
}

/**
 * The configuration of a screen, from the attributes of its `.app` element: the stream's and the
 * session check's addresses, the last event the page shows (`data-after`), and the change kinds
 * (`data-changed-kinds`, separated by white space; absent: every change).
 * @param {import('./view-config.js').DataAttributes} data
 * @returns {ScreenConfig}
 * @throws {ContractError}
 */
export function parseScreenConfig(data) {
  const after = (data.after ?? '').trim();
  const cursor = /^\d+$/.test(after) ? Number(after) : Number.NaN;
  if (!Number.isSafeInteger(cursor)) {
    throw new ContractError('dataset.after', 'a whole number from 0');
  }
  const listed = data.changedKinds;
  return {
    stream: { streamUrl: localPath(data, 'streamUrl'), sessionUrl: localPath(data, 'sessionUrl'), after: cursor },
    kinds: listed === undefined ? null : new Set(listed.split(/\s+/).filter((kind) => kind !== '')),
  };
}

/**
 * Whether the focus is on a form control inside `region`.
 * @param {Element} region
 * @returns {boolean}
 */
function focusInForm(region) {
  const active = region.ownerDocument.activeElement;
  return active !== null && region.contains(active) && isFormControl(active);
}

/**
 * Whether an element is a form control, or inside a form.
 * @param {Element} element
 * @returns {boolean}
 */
function isFormControl(element) {
  return element.closest('form') !== null || element.matches('input, textarea, select');
}

/**
 * Whether a select's chosen options differ from those it was rendered with.
 * @param {HTMLSelectElement} select
 * @returns {boolean}
 */
function selectEdited(select) {
  const options = Array.from(select.options);
  if (select.multiple || select.size > 1) {
    return options.some((option) => option.selected !== option.defaultSelected);
  }
  const marked = options.map((option) => option.defaultSelected).lastIndexOf(true);
  const rendered = marked === -1 && options.length > 0 ? 0 : marked;
  return select.selectedIndex !== rendered;
}

/**
 * Whether a control holds a value other than the one it was rendered with.
 * @param {Element} control
 * @returns {boolean}
 */
function controlEdited(control) {
  if (control instanceof HTMLInputElement) {
    return control.type === 'checkbox' || control.type === 'radio'
      ? control.checked !== control.defaultChecked
      : control.value !== control.defaultValue;
  }
  if (control instanceof HTMLTextAreaElement) {
    return control.value !== control.defaultValue;
  }
  return control instanceof HTMLSelectElement && selectEdited(control);
}

/**
 * Whether a control inside `region` holds a value other than the one it was rendered with.
 * @param {Element} region
 * @returns {boolean}
 */
function hasEdits(region) {
  return Array.from(region.querySelectorAll('input, textarea, select')).some(controlEdited);
}

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
 * Calls one callback of every listener; one that throws does not keep the others from theirs.
 * @param {ReadonlySet<StreamListener>} listeners
 * @param {(listener: StreamListener) => void} call
 * @returns {void}
 */
function fanOut(listeners, call) {
  for (const listener of [...listeners]) {
    try {
      call(listener);
    } catch (error) {
      console.error('dibs: a listener of the stream failed', error);
    }
  }
}

/** @type {WeakSet<Element>} */
const mounted = new WeakSet();

/** @returns {Screen} */
function inert() {
  return { teardown: () => {}, onNavigate: () => {}, subscribe: () => () => {} };
}

/**
 * Keeps the screen's regions current while events arrive. Mounting the same element again
 * answers a screen that does nothing.
 * @param {Element} app the `.app` element
 * @param {ScreenDeps} deps
 * @returns {Screen}
 */
export function mountScreen(app, deps) {
  if (mounted.has(app) || !(app instanceof HTMLElement)) {
    return inert();
  }
  /** @type {ScreenConfig} */
  let config;
  try {
    config = parseScreenConfig(app.dataset);
  } catch (error) {
    console.error('dibs: the screen is missing its live configuration', error);
    return inert();
  }
  mounted.add(app);
  const { fetchRegions, location, onSwap } = deps;
  const doc = app.ownerDocument;
  const controller = new AbortController();
  const { signal } = controller;
  const stream = new AbortController();
  /** @type {Set<StreamListener>} */
  const listeners = new Set();
  let ended = false;
  let owed = false;
  let running = false;
  let again = false;
  /** The regions the last refresh held, because the person was using them. @type {Set<string>} */
  let held = new Set();
  /** @type {AbortController | null} */
  let inflight = null;

  const showChanged = () => {
    doc.querySelectorAll('[data-changed] a[href]').forEach((link) => link.setAttribute('href', location.href));
    setHidden(doc, '[data-changed]', false);
  };

  const end = () => {
    if (ended) {
      return;
    }
    ended = true;
    refreshes.cancel();
    inflight?.abort();
    stream.abort();
    setHidden(doc, '[data-changed]', true);
    setHidden(doc, '[data-session-ended]', false);
    fanOut(listeners, (listener) => listener.onState?.('ended'));
  };

  /**
   * Swaps one region in, keeping its scroll position and the focus that was inside it.
   * @param {Element} current
   * @param {Element} incoming
   */
  const swap = (current, incoming) => {
    const active = doc.activeElement;
    const was = active !== null && active !== current && current.contains(active)
      ? { id: active.id, href: active.getAttribute('href') ?? '' }
      : null;
    const { scrollTop, scrollLeft } = current;
    const next = swapRegion(current, incoming);
    next.scrollTop = scrollTop;
    next.scrollLeft = scrollLeft;
    if (was !== null) {
      const candidates = Array.from(next.querySelectorAll('[id], a[href]'));
      const target = candidates[
        focusIndex(was, candidates.map((element) => ({ id: element.id, href: element.getAttribute('href') ?? '' })))
      ];
      if (target instanceof HTMLElement) {
        target.focus({ preventScroll: true });
      }
    }
    onSwap(next);
  };

  /**
   * Swaps in the regions of an answer, region by region.
   * @param {Map<string, Element>} regions
   * @param {readonly string[]} names
   */
  const apply = (regions, names) => {
    /** @type {Set<string>} */
    const holding = new Set();
    /** @type {{ [A in RegionAction]: (current: Element, incoming: Element, name: string) => void }} */
    const act = {
      swap,
      skip: () => {},
      hold: (_current, _incoming, name) => {
        holding.add(name);
      },
    };
    for (const name of names) {
      const current = findRegion(app, name);
      const incoming = regions.get(name);
      if (current === null || incoming === undefined) {
        continue;
      }
      const action = regionAction({
        ownLive: current.getAttribute('data-live') === 'own',
        focusInside: focusInForm(current),
        edited: hasEdits(current),
      });
      act[action](current, incoming, name);
    }
    held = holding;
    if (held.size > 0) {
      showChanged();
    } else {
      setHidden(doc, '[data-changed]', true);
    }
  };

  /**
   * What an answer without regions leads to.
   * @type {{ [K in Exclude<import('./regions.js').RegionsAnswer['kind'], 'ok'>]: () => void }}
   */
  const unanswered = {
    'signed-out': end,
    failed: showChanged,
  };

  const refresh = async () => {
    const names = SCREEN_REGIONS.filter((name) => findRegion(app, name) !== null);
    if (names.length === 0) {
      return;
    }
    const fetchedFor = location.href;
    const mine = new AbortController();
    inflight = mine;
    /** @type {import('./regions.js').RegionsAnswer} */
    let answer;
    try {
      answer = await fetchRegions(fetchedFor, names, { signal: AbortSignal.any([signal, mine.signal]) });
    } catch (error) {
      if (mine.signal.aborted || signal.aborted) {
        return;
      }
      console.error('dibs: the screen could not be refreshed', error);
      showChanged();
      return;
    } finally {
      if (inflight === mine) {
        inflight = null;
      }
    }
    if (ended || mine.signal.aborted || signal.aborted) {
      return;
    }
    if (!answerStillWanted(fetchedFor, location.href)) {
      refreshes.request();
      return;
    }
    if (answer.kind === 'ok') {
      apply(answer.regions, names);
      return;
    }
    unanswered[answer.kind]();
  };

  const start = () => {
    if (running) {
      again = true;
      return;
    }
    running = true;
    refresh()
      .catch((error) => {
        console.error('dibs: the screen could not be refreshed', error);
      })
      .finally(() => {
        running = false;
        if (again && !ended && !signal.aborted) {
          again = false;
          refreshes.request();
        }
      });
  };

  /** @type {{ [K in import('./refresh-timing.js').DueAction]: () => void }} */
  const due = {
    fetch: start,
    owe: () => {
      owed = true;
    },
    stop: () => {},
  };

  const refreshes = coalesce({
    windowMs: BURST_MS,
    timer: {
      set: (run, ms) => setTimeout(run, ms),
      clear: (handle) => clearTimeout(handle),
    },
    run: () => due[whenDue({ ended, hidden: doc.hidden })](),
  });

  app.addEventListener('focusout', (event) => {
    if (held.size === 0 || ended) {
      return;
    }
    const next = event instanceof FocusEvent && event.relatedTarget instanceof Element ? event.relatedTarget : null;
    const inUse = [...held].some((name) => {
      const region = findRegion(app, name);
      return region !== null && (hasEdits(region) || (next !== null && region.contains(next) && isFormControl(next)));
    });
    if (!inUse) {
      held = new Set();
      refreshes.request();
    }
  }, { signal });

  doc.addEventListener('visibilitychange', () => {
    if (!doc.hidden && owed && !ended) {
      owed = false;
      refreshes.request();
    }
  }, { signal });

  follow({
    config: config.stream,
    signal: AbortSignal.any([signal, stream.signal]),
    onEvent: (event) => {
      if (!ended && isRelevant(event, config.kinds)) {
        refreshes.request();
      }
      fanOut(listeners, (listener) => listener.onEvent?.(event));
    },
    onState: (state) => {
      if (state === 'ended') {
        end();
        return;
      }
      fanOut(listeners, (listener) => listener.onState?.(state));
    },
    onResync: () => {
      if (!ended) {
        refreshes.request();
      }
      fanOut(listeners, (listener) => listener.onResync?.());
    },
  });

  return {
    teardown: () => {
      controller.abort();
      refreshes.cancel();
      inflight?.abort();
      listeners.clear();
      mounted.delete(app);
    },
    onNavigate: () => {
      if (inflight !== null) {
        inflight.abort();
        if (!ended) {
          refreshes.request();
        }
      }
    },
    subscribe: (listener) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
}
