// @ts-check
/**
 * A live conversation: messages posted to it are appended as they arrive, cloned from the page's
 * `<template>` and filled with `textContent` — what dibs wrote marked as such, named as the page
 * says and without a session — a raised importance shown with the template's badge for it and, for
 * a message that asks the person to acknowledge it, the template's acknowledge form; a pill counts
 * them and links to the first; an acknowledgement marks its recipient as having acknowledged in
 * the message's list, cloned from the page's `<template>` for it, and takes the person's own
 * acknowledge button away once they acknowledged; a notice shows while the connection is restored.
 * When the log no longer reaches back to what the page shows, the page loads again, unless a reply
 * is being written. Every wording comes from the template.
 *
 * A message appended live is then replaced by the form the server renders it in, with the
 * references in it linked: once the burst of messages settles, the page at the address shown is
 * asked for once more, one request at a time, and each message appended live is swapped for the
 * element of the same `id` in the answer. Nothing else is replaced, so a reply being written and
 * the scroll position stay; when the answer fails, lacks the message, or the page's address changed
 * meanwhile, the message stays as appended.
 *
 * Given the screen's stream, the conversation follows it and leaves the page's notices to the
 * screen; without it, the conversation follows the stream itself and shows the session-ended notices
 * inside it when the session ends.
 */

import { coalesce } from './coalesce.js';
import { ackOf, messageOf } from './events.js';
import { follow } from './live-stream.js';
import { localizeTime, localizeTimes } from './local-times.js';
import { ackAction, offersAck, showsImportance } from './message-marks.js';
import { answerStillWanted } from './screen-view.js';
import { countLabel, parseThreadConfig } from './view-config.js';

/** How long the messages of one burst are gathered before their rendered form is asked for, in milliseconds. */
const BURST_MS = 500;

/** The region of the conversation's page the rendered messages are read from. */
const RENDERED_REGIONS = Object.freeze(['inspector']);

/**
 * What a conversation is given, each part optional.
 * @typedef {object} ThreadDeps
 * @property {(listener: import('./screen-view.js').StreamListener) => () => void} [subscribe] the
 *   screen's stream; without it, the conversation follows the stream itself
 * @property {import('./regions.js').FetchRegions} [fetchRegions] reads the conversation's page
 *   again for the rendered form of the messages appended live; without it, they stay as appended
 * @property {Pick<Location, 'href'>} [location] the address the page shows
 */

/**
 * What an answer settles for the messages appended live: those of `asked` that `rendered` holds are
 * replaced; every one the answer was asked for is settled, so only those appended since it was
 * asked for still wait.
 * @param {readonly number[]} waiting the messages waiting for their rendered form now
 * @param {readonly number[]} asked the messages the answer was asked for
 * @param {ReadonlySet<number>} rendered the messages the answer holds
 * @returns {{ replace: number[], waiting: number[] }}
 */
export function settle(waiting, asked, rendered) {
  const settled = new Set(asked);
  return {
    replace: asked.filter((id) => rendered.has(id)),
    waiting: waiting.filter((id) => !settled.has(id)),
  };
}

/**
 * The message an element's `id` anchors (`m-{id}`); `null` for any other `id`.
 * @param {string} anchor
 * @returns {number | null}
 */
export function messageOfAnchor(anchor) {
  const match = /^m-(\d+)$/.exec(anchor);
  const id = match === null ? Number.NaN : Number(match[1]);
  return Number.isSafeInteger(id) ? id : null;
}

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

/** @type {WeakSet<HTMLElement>} */
const mounted = new WeakSet();

/**
 * @template {Element} T
 * @param {ParentNode} root
 * @param {string} selector
 * @param {new (...args: never[]) => T} type
 * @returns {T | null}
 */
function find(root, selector, type) {
  const found = root.querySelector(selector);
  return found instanceof type ? found : null;
}

/**
 * Mounts the live behaviour on a conversation's root. Mounting the same element again does nothing.
 * @param {HTMLElement} root the element carrying `data-thread-view`
 * @param {ThreadDeps} [deps]
 * @returns {() => void} the teardown
 */
export function mount(root, deps = {}) {
  if (mounted.has(root)) {
    return () => {};
  }
  let config;
  try {
    config = parseThreadConfig(root.dataset);
  } catch (error) {
    console.error('dibs: the thread page is missing its configuration', error);
    return () => {};
  }
  const list = find(root, '[data-messages]', HTMLElement);
  const template = find(root, 'template[data-message-template]', HTMLTemplateElement);
  const ackTemplate = find(root, 'template[data-ack-done-template]', HTMLTemplateElement);
  const pill = find(root, '[data-live-pill]', HTMLAnchorElement);
  const pillText = find(root, '[data-live-pill-text]', HTMLElement);
  const notice = find(root, '[data-conn]', HTMLElement);
  if (
    list === null ||
    template === null ||
    ackTemplate === null ||
    pill === null ||
    pillText === null ||
    notice === null
  ) {
    console.error('dibs: the thread page is missing a part the live view needs');
    return () => {};
  }
  mounted.add(root);
  const controller = new AbortController();
  const { signal } = controller;
  const thread = config.thread;
  const { subscribe, fetchRegions, location } = deps;
  let fresh = 0;
  /** The messages appended live that wait for their rendered form. @type {number[]} */
  let waiting = [];
  let running = false;
  let again = false;

  /**
   * Asks for the conversation's page once and swaps each message waiting for its rendered form.
   * @returns {Promise<void>}
   */
  const render = async () => {
    if (fetchRegions === undefined || location === undefined || waiting.length === 0) {
      return;
    }
    const asked = [...waiting];
    const fetchedFor = location.href;
    /** @type {Map<number, Element>} */
    let incoming = new Map();
    try {
      const answer = await fetchRegions(fetchedFor, RENDERED_REGIONS, { signal });
      const region = answer.kind === 'ok' ? answer.regions.get('inspector') : undefined;
      if (region !== undefined && answerStillWanted(fetchedFor, location.href)) {
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
    for (const id of settled.replace) {
      const current = document.getElementById(`m-${id}`);
      const next = incoming.get(id);
      if (current !== null && next !== undefined && root.contains(current)) {
        const copy = document.importNode(next, true);
        current.replaceWith(copy);
        localizeTimes(copy);
      }
    }
  };

  const startRender = () => {
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
    run: startRender,
  });

  /** @param {import('./events.js').StreamMessage} message */
  const append = (message) => {
    const anchor = `m-${message.id}`;
    if (document.getElementById(anchor) !== null) {
      return;
    }
    const item = template.content.firstElementChild?.cloneNode(true);
    if (!(item instanceof HTMLElement)) {
      return;
    }
    item.id = anchor;
    item.classList.toggle('msg--mine', !message.system && message.author === config.me);
    item.classList.toggle('msg--system', message.system);
    const fill = (/** @type {string} */ field, /** @type {string} */ value) => {
      const target = item.querySelector(`[data-field="${field}"]`);
      if (target !== null) {
        target.textContent = value;
      }
    };
    fill('author', message.system ? config.systemAuthor : message.author);
    if (message.system) {
      item.querySelector('[data-field="session"]')?.remove();
    } else {
      fill('session', message.label);
    }
    fill('body', message.body);
    if (showsImportance(message.importance)) {
      const badge = item.querySelector(`[data-importance="${message.importance}"]`);
      if (badge instanceof HTMLElement) {
        badge.hidden = false;
      }
    }
    const ack = item.querySelector('[data-ack]');
    if (message.ackRequired && ack instanceof HTMLElement) {
      ack.hidden = false;
    }
    const ackForm = item.querySelector('form[data-ack-form]');
    if (offersAck(message, config.me, config.sender) && ackForm instanceof HTMLFormElement) {
      ackForm.action = ackAction(config.ackAction, message.id);
      ackForm.hidden = false;
    }
    const time = item.querySelector('time[data-field="time"]');
    if (time !== null) {
      time.setAttribute('datetime', message.createdAt);
      localizeTime(time, new Date());
    }
    list.append(item);
    if (fetchRegions !== undefined && location !== undefined) {
      waiting.push(message.id);
      renders.request();
    }
    fresh += 1;
    if (fresh === 1) {
      pill.href = `#${anchor}`;
    }
    pillText.textContent = countLabel(fresh, config.newMessageLabel, config.newMessagesLabel);
    pill.hidden = false;
  };

  /** @param {import('./events.js').StreamAck} acked */
  const markAcked = (acked) => {
    const item = document.getElementById(`m-${acked.message}`);
    const acks = item !== null && root.contains(item) ? item.querySelector('[data-ack-list]') : null;
    const entry = ackTemplate.content.firstElementChild?.cloneNode(true);
    if (item === null || acks === null || !(entry instanceof HTMLElement)) {
      return;
    }
    entry.dataset.ackMember = acked.member;
    const name = entry.querySelector('[data-field="name"]');
    if (name !== null) {
      name.textContent = acked.member;
    }
    const you = entry.querySelector('[data-field="you"]');
    if (you instanceof HTMLElement) {
      if (acked.member === config.me) {
        you.hidden = false;
      } else {
        you.remove();
      }
    }
    const time = entry.querySelector('time[data-field="time"]');
    if (time !== null) {
      time.setAttribute('datetime', acked.at);
      localizeTime(time, new Date());
    }
    const listed = Array.from(acks.querySelectorAll('[data-ack-member]')).find(
      (element) => element instanceof HTMLElement && element.dataset.ackMember === acked.member,
    );
    if (listed === undefined) {
      acks.append(entry);
    } else {
      listed.replaceWith(entry);
    }
    if (acked.member === config.me) {
      item.querySelectorAll('form[data-ack-form]').forEach((form) => form.remove());
    }
  };

  pill.addEventListener('click', () => {
    pill.hidden = true;
    fresh = 0;
  }, { signal });

  /** @param {import('./events.js').StreamEvent} event */
  const onEvent = (event) => {
    const message = messageOf(event, thread);
    if (message !== null) {
      append(message);
    }
    const acked = ackOf(event, thread);
    if (acked !== null) {
      markAcked(acked);
    }
  };

  /** @param {import('./live-stream.js').StreamState} state */
  const onState = (state) => {
    notice.hidden = state !== 'reconnecting';
    if (state !== 'ended') {
      return;
    }
    renders.cancel();
    if (subscribe === undefined) {
      root.querySelectorAll('[data-session-ended]').forEach((element) => {
        if (element instanceof HTMLElement) {
          element.hidden = false;
        }
      });
    }
  };

  const onResync = () => {
    const draft = find(root, 'textarea[name="body"]', HTMLTextAreaElement);
    if (draft === null || draft.value.trim() === '') {
      window.location.reload();
    }
  };

  /** @type {() => void} */
  let unsubscribe = () => {};
  if (subscribe === undefined) {
    follow({ config, signal, onEvent, onState, onResync });
  } else {
    unsubscribe = subscribe({ onEvent, onState, onResync });
  }

  return () => {
    unsubscribe();
    renders.cancel();
    controller.abort();
    mounted.delete(root);
  };
}
