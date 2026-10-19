// @ts-check
/**
 * The live thread page: messages posted to the open thread are appended as they arrive, cloned
 * from the page's `<template>` and filled with `textContent`; a pill counts them and links to the
 * first; a notice shows while the connection is restored, and the page's session-ended banner when
 * the session is gone. Every wording comes from the template.
 */

import { messageOf } from './events.js';
import { follow } from './live-stream.js';
import { localizeTime } from './local-times.js';
import { countLabel, parseThreadConfig } from './view-config.js';

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
 * Mounts the live behaviour on a thread page's root. Mounting the same element again does nothing.
 * @param {HTMLElement} root the element carrying `data-thread-view`
 * @returns {() => void} the teardown
 */
export function mount(root) {
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
  const pill = find(root, '[data-live-pill]', HTMLAnchorElement);
  const pillText = find(root, '[data-live-pill-text]', HTMLElement);
  const notice = find(root, '[data-conn]', HTMLElement);
  if (list === null || template === null || pill === null || pillText === null || notice === null) {
    console.error('dibs: the thread page is missing a part the live view needs');
    return () => {};
  }
  mounted.add(root);
  const controller = new AbortController();
  const { signal } = controller;
  const thread = config.thread;
  let fresh = 0;

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
    item.classList.toggle('msg--mine', message.author === config.me);
    const fill = (/** @type {string} */ field, /** @type {string} */ value) => {
      const target = item.querySelector(`[data-field="${field}"]`);
      if (target !== null) {
        target.textContent = value;
      }
    };
    fill('author', message.author);
    fill('session', message.label);
    fill('body', message.body);
    const time = item.querySelector('time[data-field="time"]');
    if (time !== null) {
      time.setAttribute('datetime', message.createdAt);
      localizeTime(time, new Date());
    }
    list.append(item);
    fresh += 1;
    if (fresh === 1) {
      pill.href = `#${anchor}`;
    }
    pillText.textContent = countLabel(fresh, config.newMessageLabel, config.newMessagesLabel);
    pill.hidden = false;
  };

  pill.addEventListener('click', () => {
    pill.hidden = true;
    fresh = 0;
  }, { signal });

  follow({
    config,
    signal,
    onEvent: (event) => {
      const message = messageOf(event, thread);
      if (message !== null) {
        append(message);
      }
    },
    onState: (state) => {
      notice.hidden = state !== 'reconnecting';
      if (state === 'ended') {
        root.querySelectorAll('[data-session-ended]').forEach((element) => {
          if (element instanceof HTMLElement) {
            element.hidden = false;
          }
        });
      }
    },
  });

  return () => {
    controller.abort();
    mounted.delete(root);
  };
}
