// @ts-check
/**
 * A message appended live is replaced by the form the server renders it in, with the references in
 * it linked: once the burst of messages settles, the page is asked for once more at the address it
 * is read from (on a page answering a form, the screen's address rather than the one the form
 * posted to), one request at a time, and each message appended live is swapped for the element of
 * the same `id` in the answer. Nothing else is replaced, so a reply being written and the scroll
 * position stay; when the answer fails, lacks the message, or the page's address changed meanwhile,
 * the message stays as appended.
 */

import { answerStillWanted } from './answer-address.js';
import { coalesce } from './coalesce.js';
import { localizeTime } from './local-times.js';
import { anchorOf, messageOfAnchor } from './message-anchors.js';
import { settle } from './rendered-messages.js';

/** How long the messages of one burst are gathered before their rendered form is asked for, in milliseconds. */
const BURST_MS = 500;

/** The region of the conversation's page the rendered messages are read from. */
const RENDERED_REGIONS = Object.freeze(['inspector']);

/**
 * @typedef {object} Rerender
 * @property {(message: number) => void} add a message was appended live and waits for its rendered form
 * @property {() => void} cancel drops the request waiting, if any
 */

/**
 * What the replacement is given.
 * @typedef {object} RerenderDeps
 * @property {HTMLElement} root the conversation
 * @property {AbortSignal} signal ends the replacement with the conversation
 * @property {{ now(): Date }} clock the time the copies' times are written against
 * @property {import('./regions.js').FetchRegions} [fetchRegions] reads the conversation's page again;
 *   without it, messages stay as appended
 * @property {import('./readable-address.js').PageAddress} [address] the address the page is read from
 */

/**
 * The messages of a rendered conversation, by id.
 * @param {Element} region
 * @returns {Map<number, Element>}
 */
function renderedMessages(region) {
  /** @type {Map<number, Element>} */
  const found = new Map();
  region.querySelectorAll('[id]').forEach((element) => {
    const id = messageOfAnchor(element.id);
    if (id !== null && !found.has(id)) {
      found.set(id, element);
    }
  });
  return found;
}

/** @returns {Rerender} */
function inert() {
  return { add: () => {}, cancel: () => {} };
}

/**
 * Replaces the messages appended live in `root` by their rendered form.
 * @param {RerenderDeps} deps
 * @returns {Rerender}
 */
export function createRerender({ root, signal, clock, fetchRegions, address }) {
  if (fetchRegions === undefined || address === undefined) {
    return inert();
  }
  const doc = root.ownerDocument;
  /** The messages appended live that wait for their rendered form. @type {number[]} */
  let waiting = [];
  let running = false;
  let again = false;

  /**
   * Asks for the conversation's page once and swaps each message waiting for its rendered form.
   * @returns {Promise<void>}
   */
  const render = async () => {
    if (waiting.length === 0) {
      return;
    }
    const asked = [...waiting];
    const fetchedFor = address.readable();
    /** @type {Map<number, Element>} */
    let incoming = new Map();
    try {
      const answer = await fetchRegions(fetchedFor, RENDERED_REGIONS, { signal });
      const region = answer.kind === 'ok' ? answer.regions.get('inspector') : undefined;
      if (region !== undefined && answerStillWanted(fetchedFor, address.readable())) {
        incoming = renderedMessages(region);
      }
    } catch (error) {
      if (signal.aborted) {
        return;
      }
      console.error('dibs: a message could not be shown in its rendered form', error);
    }
    if (signal.aborted) {
      return;
    }
    const settled = settle(waiting, asked, new Set(incoming.keys()));
    waiting = settled.waiting;
    const now = clock.now();
    for (const id of settled.replace) {
      const current = doc.getElementById(anchorOf(id));
      const next = incoming.get(id);
      if (current !== null && next !== undefined && root.contains(current)) {
        const copy = doc.importNode(next, true);
        current.replaceWith(copy);
        copy.querySelectorAll('time[data-clock]').forEach((time) => localizeTime(time, now));
      }
    }
  };

  const start = () => {
    if (running) {
      again = true;
      return;
    }
    running = true;
    render()
      .catch((error) => {
        console.error('dibs: a message could not be shown in its rendered form', error);
      })
      .finally(() => {
        running = false;
        const more = again && waiting.length > 0 && !signal.aborted;
        again = false;
        if (more) {
          renders.request();
        }
      });
  };

  const renders = coalesce({
    windowMs: BURST_MS,
    timer: {
      set: (run, ms) => setTimeout(run, ms),
      clear: (handle) => clearTimeout(handle),
    },
    run: start,
  });

  return {
    add: (message) => {
      waiting.push(message);
      renders.request();
    },
    cancel: () => renders.cancel(),
  };
}
