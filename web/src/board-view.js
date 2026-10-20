// @ts-check
/**
 * The live board: when an event of the kinds the page names arrives, the columns are fetched again
 * from the page's fragment address and swapped in, once per burst of events and never while the
 * tab is hidden; a fetch that came due while it was hidden runs once when it shows again. While
 * the person is using a form in the columns, the page's reload banner shows instead and the
 * columns are left as they are. The time the columns were rendered at travels with them, and the
 * page's slots for it show it after a swap. When the session is gone, the page's session-ended banner shows
 * and nothing is fetched any more. Every wording comes from the template.
 *
 * The answer is swapped in only when it is the board's own fragment: a `200` HTML answer carrying
 * the fragment header, fetched without following a redirect. A newer fetch aborts the one in
 * flight, and every fetch has a deadline.
 */

import { onAnswer, whenDue } from './board-refresh.js';
import { coalesce } from './coalesce.js';
import { isChange } from './events.js';
import { follow } from './live-stream.js';
import { localizeTimes } from './local-times.js';
import { parseBoardConfig } from './view-config.js';

/** How long the events of one burst are gathered before one fetch, in milliseconds. */
const BURST_MS = 500;

/** How long a fetch of the columns may take, in milliseconds. */
const FETCH_DEADLINE_MS = 5000;

/** The header the fragment route names its answer in. */
const FRAGMENT_HEADER = 'X-Dibs-Fragment';

/** @type {WeakSet<HTMLElement>} */
const mounted = new WeakSet();

/**
 * @param {HTMLElement} root
 * @param {string} selector
 * @param {boolean} hidden
 * @returns {void}
 */
function setHidden(root, selector, hidden) {
  root.querySelectorAll(selector).forEach((element) => {
    if (element instanceof HTMLElement) {
      element.hidden = hidden;
    }
  });
}

/**
 * Whether the person is using a form inside `region`: a field or button of one has focus, a field
 * of one holds text, or a take-over is open.
 * @param {HTMLElement} region
 * @returns {boolean}
 */
function inUse(region) {
  const active = document.activeElement;
  if (active instanceof Element && region.contains(active) && active.closest('form, details[open]') !== null) {
    return true;
  }
  if (region.querySelector('details[open]') !== null) {
    return true;
  }
  return Array.from(region.querySelectorAll('form textarea, form input:not([type="hidden"])')).some(
    (field) =>
      (field instanceof HTMLTextAreaElement || field instanceof HTMLInputElement) && field.value.trim() !== '',
  );
}

/**
 * Mounts the live behaviour on the board's root. Mounting the same element again does nothing.
 * @param {HTMLElement} root the element carrying `data-board-view`
 * @returns {() => void} the teardown
 */
export function mount(root) {
  if (mounted.has(root)) {
    return () => {};
  }
  let config;
  try {
    config = parseBoardConfig(root.dataset);
  } catch (error) {
    console.error('dibs: the board is missing its live configuration', error);
    return () => {};
  }
  const region = root.querySelector('[data-board-region]');
  if (!(region instanceof HTMLElement)) {
    console.error('dibs: the board is missing the region its columns are swapped into');
    return () => {};
  }
  mounted.add(root);
  const controller = new AbortController();
  const { signal } = controller;
  const { fragmentUrl, kinds } = config;
  let ended = false;
  let owed = false;
  /** @type {AbortController | null} */
  let inflight = null;

  const offerReload = () => setHidden(root, '[data-changed]', false);

  const end = () => {
    ended = true;
    refreshes.cancel();
    inflight?.abort();
    setHidden(root, '[data-changed]', true);
    setHidden(root, '[data-session-ended]', false);
  };

  const showRenderedAt = () => {
    const rendered = region.querySelector('template[data-updated-template]');
    if (!(rendered instanceof HTMLTemplateElement)) {
      return;
    }
    root.querySelectorAll('[data-updated-slot]').forEach((slot) => {
      slot.replaceChildren(rendered.content.cloneNode(true));
      localizeTimes(slot);
    });
  };

  /** @param {string} markup the fragment, escaped by the server that rendered it */
  const swap = (markup) => {
    region.innerHTML = markup;
    localizeTimes(region);
    showRenderedAt();
    setHidden(root, '[data-changed]', true);
  };

  /** @type {{ [K in import('./board-refresh.js').AnswerAction]: (markup: string) => void }} */
  const answered = {
    swap,
    hold: offerReload,
    ended: end,
    failed: offerReload,
  };

  const fetchColumns = async () => {
    inflight?.abort();
    const mine = new AbortController();
    inflight = mine;
    try {
      const response = await fetch(fragmentUrl, {
        credentials: 'same-origin',
        redirect: 'error',
        cache: 'no-store',
        headers: { Accept: 'text/html' },
        signal: AbortSignal.any([signal, mine.signal, AbortSignal.timeout(FETCH_DEADLINE_MS)]),
      });
      const action = onAnswer(
        {
          status: response.status,
          contentType: response.headers.get('Content-Type') ?? '',
          fragment: response.headers.get(FRAGMENT_HEADER) ?? '',
        },
        inUse(region),
      );
      const markup = action === 'swap' ? await response.text() : '';
      if (!mine.signal.aborted && !signal.aborted) {
        answered[action](markup);
      }
    } catch (error) {
      if (mine.signal.aborted || signal.aborted) {
        return;
      }
      console.error('dibs: the board could not be refreshed', error);
      offerReload();
    } finally {
      if (inflight === mine) {
        inflight = null;
      }
    }
  };

  /** @type {{ [K in import('./board-refresh.js').DueAction]: () => void }} */
  const due = {
    fetch: () => {
      fetchColumns().catch((error) => {
        console.error('dibs: the board could not be refreshed', error);
      });
    },
    owe: () => {
      owed = true;
    },
    hold: offerReload,
    stop: () => {},
  };

  const refreshes = coalesce({
    windowMs: BURST_MS,
    timer: {
      set: (run, ms) => setTimeout(run, ms),
      clear: (handle) => clearTimeout(handle),
    },
    run: () => due[whenDue({ ended, hidden: document.hidden, typing: inUse(region) })](),
  });

  document.addEventListener('visibilitychange', () => {
    if (!document.hidden && owed && !ended) {
      owed = false;
      refreshes.request();
    }
  }, { signal });

  follow({
    config,
    signal,
    onEvent: (event) => {
      if (!ended && isChange(event, kinds)) {
        refreshes.request();
      }
    },
    onState: (state) => {
      if (state === 'ended') {
        end();
      }
    },
  });

  return () => {
    controller.abort();
    refreshes.cancel();
    mounted.delete(root);
  };
}
