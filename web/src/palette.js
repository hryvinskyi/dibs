// @ts-check
/**
 * The search palette: a dialog over the page that searches the project as the person types and
 * opens a hit here or outside, from the keyboard. It opens on `⌘K` or `Ctrl K`, on `/` outside a
 * field, and on a click or a character typed in the rail's search box, carrying what was typed
 * there; focus alone never opens it, since closing it gives the focus back to where it was. The
 * hits are the server's: after a pause in the typing the palette asks the search for its compact
 * answer and shows a copy of its results region. The rail's search box still submits without
 * scripts, and so does the palette's when it has no hit to act on.
 *
 * Every word shown comes from the markup; off a Mac, the keys' labels that name the command key
 * are rewritten with their `data-kbd-other` text.
 */

import { findRegion } from './regions.js';
import { isMac, next } from './palette-keys.js';

/** How long the typing must pause before the palette asks the server, in milliseconds. */
const TYPING_PAUSE_MS = 200;

/** The region the palette asks the search for. */
const RESULTS = Object.freeze(['results']);

/** What counts as a field, where `/` is something typed. */
const FIELD = 'input, textarea, select, [contenteditable]:not([contenteditable="false"])';

/** The rail's search box. */
const RAIL_INPUT = 'form[data-search] input[name="q"]';

/**
 * What the palette is given.
 * @typedef {object} PaletteDeps
 * @property {import('./regions.js').FetchRegions} fetchRegions
 * @property {Pick<Location, 'assign'>} location
 * @property {(url: string, target: string, features: string) => unknown} open window.open
 * @property {() => void} onSignedOut called when the session turns out to have ended
 */

/**
 * The address of the palette's answer for a query.
 * @param {string} query
 * @returns {string}
 */
function answerAddress(query) {
  return `/ui/search?q=${encodeURIComponent(query)}&palette=1`;
}

/**
 * The platform the browser names, for the keys' labels.
 * @param {Window | null} view
 * @returns {string}
 */
function platformOf(view) {
  /** @type {(Navigator & { userAgentData?: { platform?: string } }) | undefined} */
  const navigator = view?.navigator;
  return navigator?.userAgentData?.platform ?? navigator?.platform ?? '';
}

/**
 * Writes the labels for another platform than the Mac into the keys inside `scope` that have one.
 * @param {ParentNode} scope
 * @returns {void}
 */
function relabel(scope) {
  scope.querySelectorAll('kbd[data-kbd-other]').forEach((kbd) => {
    const other = kbd.getAttribute('data-kbd-other');
    if (other !== null && kbd.textContent !== other) {
      kbd.textContent = other;
    }
  });
}

/** @type {WeakSet<Element>} */
const mounted = new WeakSet();

/**
 * Makes the palette rendered after `app` work. Mounting the same palette again does nothing.
 * @param {Document} doc the page's document, which holds the palette's dialog
 * @param {Element} app the `.app` element, which holds the rail
 * @param {PaletteDeps} deps
 * @returns {() => void} teardown
 */
export function mountPalette(doc, app, { fetchRegions, location, open, onSignedOut }) {
  const dialog = doc.querySelector('dialog[data-palette]');
  const input = dialog?.querySelector('input[data-palette-input]');
  const results = dialog?.querySelector('[data-palette-results]');
  if (
    !(dialog instanceof HTMLDialogElement) ||
    !(input instanceof HTMLInputElement) ||
    !(results instanceof HTMLElement) ||
    mounted.has(dialog)
  ) {
    return () => {};
  }
  mounted.add(dialog);
  const controller = new AbortController();
  const { signal } = controller;
  const hint = Array.from(results.childNodes, (node) => node.cloneNode(true));
  let index = 0;
  /** @type {Element | null} */
  let returnTo = null;
  /** @type {ReturnType<typeof setTimeout> | undefined} */
  let timer;
  /** @type {AbortController | null} */
  let inflight = null;

  /** @returns {HTMLElement[]} the hits listed */
  const hits = () =>
    Array.from(results.querySelectorAll('[data-hit]')).filter((hit) => hit instanceof HTMLElement);

  /**
   * Selects the hit at `at`: marks it, points the input at it and scrolls it into view.
   * @param {number} at
   */
  const select = (at) => {
    index = at;
    const listed = hits();
    listed.forEach((hit, position) => {
      hit.setAttribute('aria-selected', position === at ? 'true' : 'false');
    });
    const selected = listed[at];
    if (selected !== undefined && selected.id !== '') {
      input.setAttribute('aria-activedescendant', selected.id);
      selected.scrollIntoView({ block: 'nearest' });
    } else {
      input.removeAttribute('aria-activedescendant');
    }
  };

  /** Shows the hint in place of any hits. */
  const showHint = () => {
    results.replaceChildren(...hint.map((node) => node.cloneNode(true)));
    select(0);
  };

  /** Stops the wait and the request in flight. */
  const halt = () => {
    clearTimeout(timer);
    timer = undefined;
    inflight?.abort();
    inflight = null;
  };

  /**
   * Asks the search for `query` and shows its hits, unless a newer query was asked meanwhile.
   * @param {string} query
   */
  const ask = async (query) => {
    const mine = new AbortController();
    inflight = mine;
    /** @type {import('./regions.js').RegionsAnswer} */
    let answer;
    try {
      answer = await fetchRegions(answerAddress(query), RESULTS, {
        signal: AbortSignal.any([signal, mine.signal]),
      });
    } catch (error) {
      if (!mine.signal.aborted && !signal.aborted) {
        console.error('dibs: the search could not be read', error);
      }
      return;
    }
    if (inflight !== mine || mine.signal.aborted || signal.aborted) {
      return;
    }
    inflight = null;
    if (answer.kind === 'signed-out') {
      onSignedOut();
      return;
    }
    if (answer.kind === 'failed') {
      return;
    }
    const region = answer.regions.get('results');
    if (region === undefined) {
      return;
    }
    results.replaceChildren(...Array.from(region.childNodes, (node) => doc.importNode(node, true)));
    select(0);
  };

  /** Searches for what the input holds once the typing pauses; an empty query shows the hint. */
  const schedule = () => {
    halt();
    const query = input.value;
    if (query.trim() === '') {
      showHint();
      return;
    }
    timer = setTimeout(() => {
      timer = undefined;
      ask(query).catch((error) => {
        console.error('dibs: the search failed', error);
      });
    }, TYPING_PAUSE_MS);
  };

  /**
   * Opens the palette with `text` in its input.
   * @param {string} text
   */
  const show = (text) => {
    if (dialog.open) {
      return;
    }
    returnTo = doc.activeElement;
    dialog.showModal();
    input.value = text;
    showHint();
    input.focus();
    if (text.trim() !== '') {
      schedule();
    }
  };

  /**
   * Opens the palette with the rail's search box's text, which leaves the box: a box left holding
   * text would keep its region from following the page's changes.
   * @param {HTMLInputElement} box
   */
  const showFromRail = (box) => {
    const text = box.value;
    box.value = box.defaultValue;
    show(text);
  };

  /** The rail's search box, as the rail now is. */
  const railInput = () => {
    const box = findRegion(app, 'rail')?.querySelector(RAIL_INPUT);
    return box instanceof HTMLInputElement ? box : null;
  };

  /**
   * What each action of a key does; `true` when the palette acted on the key.
   * @type {{ [A in import('./palette-keys.js').PaletteAction]: (at: number) => boolean }}
   */
  const actions = {
    none: () => false,
    open: () => {
      show('');
      return true;
    },
    close: () => {
      dialog.close();
      return true;
    },
    move: (at) => {
      select(at);
      return true;
    },
    here: (at) => {
      const link = hits()[at]?.querySelector('a[data-hit-here]');
      if (!(link instanceof HTMLAnchorElement)) {
        return false;
      }
      location.assign(link.href);
      return true;
    },
    out: (at) => {
      const link = hits()[at]?.querySelector('a[data-hit-out]');
      if (!(link instanceof HTMLAnchorElement)) {
        return false;
      }
      open(link.href, '_blank', 'noopener');
      return true;
    },
  };

  doc.addEventListener('keydown', (event) => {
    if (event.defaultPrevented || event.isComposing) {
      return;
    }
    const step = next(
      {
        open: dialog.open,
        typing: event.target instanceof Element && event.target.closest(FIELD) !== null,
        count: hits().length,
        index,
      },
      event.key,
      { meta: event.metaKey, ctrl: event.ctrlKey },
    );
    if (actions[step.action](step.index)) {
      event.preventDefault();
    }
  }, { signal });

  app.addEventListener('click', (event) => {
    const box = event.target instanceof Element ? event.target.closest(RAIL_INPUT) : null;
    if (box instanceof HTMLInputElement && box === railInput()) {
      showFromRail(box);
    }
  }, { signal });

  app.addEventListener('input', (event) => {
    const box = event.target;
    if (box instanceof HTMLInputElement && box === railInput() && box.value !== '') {
      showFromRail(box);
    }
  }, { signal });

  input.addEventListener('input', schedule, { signal });

  dialog.addEventListener('click', (event) => {
    if (event.target === dialog) {
      dialog.close();
    }
  }, { signal });

  dialog.addEventListener('close', () => {
    // The close event arrives after the keys already queued behind the closing key; a palette one of
    // them opened again is left as it is.
    if (dialog.open) {
      return;
    }
    halt();
    const back = returnTo !== null && returnTo.isConnected ? returnTo : railInput();
    returnTo = null;
    if (back instanceof HTMLElement) {
      back.focus();
    }
  }, { signal });

  /** @type {MutationObserver | null} */
  let observer = null;
  if (!isMac(platformOf(doc.defaultView))) {
    relabel(doc);
    observer = new MutationObserver(() => relabel(app));
    observer.observe(app, { childList: true, subtree: true });
  }

  return () => {
    controller.abort();
    halt();
    observer?.disconnect();
    if (dialog.open) {
      dialog.close();
    }
    mounted.delete(dialog);
  };
}
