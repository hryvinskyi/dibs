// @ts-check
/**
 * A live conversation: messages posted to it are appended as they arrive, an acknowledgement marks
 * its recipient in the message's list, a pill counts the messages appended and links to the first,
 * and a notice shows while the connection is restored. When the log no longer reaches back to what
 * the page shows, the page loads again from the address it is read from, unless a reply is being
 * written. What each message and
 * acknowledgement shows is decided by `message-view.js`, built by `message-elements.js`, and later
 * replaced by its rendered form by `message-rerender.js`; times are written against the clock given.
 *
 * The conversation starts from the cursor its markup was rendered at (`data-after`): every event
 * after it is applied once, those the page's stream delivered before the conversation was mounted
 * as well as those it delivers later, and a message the page already shows is not appended again.
 *
 * Given the screen's stream, the conversation follows it and leaves the page's notices to the
 * screen; without it, the conversation follows the stream itself and shows the session-ended notices
 * inside it when the session ends.
 */

import { ackOf, messageOf } from './events.js';
import { follow } from './live-stream.js';
import { buildMessage, showAck } from './message-elements.js';
import { createRerender } from './message-rerender.js';
import { ackEntryView, messageView } from './message-view.js';
import { countLabel, parseThreadConfig } from './view-config.js';

/**
 * What a conversation is given.
 * @typedef {object} ThreadDeps
 * @property {{ now(): Date }} clock the time the conversation's times are written against
 * @property {import('./stream-catch-up.js').SubscribeAfter} [subscribe] the screen's stream, from a
 *   cursor on; without it, the conversation follows the stream itself
 * @property {import('./regions.js').FetchRegions} [fetchRegions] reads the conversation's page
 *   again for the rendered form of the messages appended live; without it, they stay as appended
 * @property {import('./readable-address.js').PageAddress} [address] the address the page is read from
 *   again; without it, the page reloads the address it shows
 */

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
 * @param {ThreadDeps} deps
 * @returns {() => void} the teardown
 */
export function mount(root, deps) {
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
  const { clock, subscribe, fetchRegions, address } = deps;
  const thread = config.thread;
  const doc = root.ownerDocument;
  const rerender = createRerender({ root, signal, clock, fetchRegions, address });
  let fresh = 0;

  /** @param {import('./events.js').StreamMessage} message */
  const append = (message) => {
    const view = messageView(message, config, clock.now());
    if (doc.getElementById(view.anchor) !== null) {
      return;
    }
    const item = buildMessage(template, view);
    if (item === null) {
      return;
    }
    list.append(item);
    rerender.add(message.id);
    fresh += 1;
    if (fresh === 1) {
      pill.href = `#${view.anchor}`;
    }
    pillText.textContent = countLabel(fresh, config.newMessageLabel, config.newMessagesLabel);
    pill.hidden = false;
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
      showAck(root, ackTemplate, acked.message, ackEntryView(acked, config.me, clock.now()));
    }
  };

  /** @param {import('./live-stream.js').StreamState} state */
  const onState = (state) => {
    notice.hidden = state !== 'reconnecting';
    if (state !== 'ended') {
      return;
    }
    rerender.cancel();
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
    if (draft !== null && draft.value.trim() !== '') {
      return;
    }
    if (address === undefined) {
      window.location.reload();
    } else {
      address.reload();
    }
  };

  /** @type {() => void} */
  let unsubscribe = () => {};
  if (subscribe === undefined) {
    follow({ config, signal, onEvent, onState, onResync });
  } else {
    unsubscribe = subscribe({ onEvent, onState, onResync }, config.after);
  }

  return () => {
    unsubscribe();
    rerender.cancel();
    controller.abort();
    mounted.delete(root);
  };
}
