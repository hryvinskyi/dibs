// @ts-check
/**
 * The live thread page: messages posted to the open thread are appended as they arrive, cloned
 * from the page's `<template>` and filled with `textContent` — what dibs wrote marked as such,
 * named as the page says and without a session — a raised importance shown with the template's
 * badge for it and, for a message that asks the person to acknowledge it, the template's
 * acknowledge form; a pill counts them and links to the first; an acknowledgement
 * marks its recipient as having acknowledged in the message's list, cloned from the page's
 * `<template>` for it, and takes the person's own acknowledge button away once they acknowledged;
 * a notice shows while the connection is restored, and the page's session-ended banner when the
 * session is gone. When the log no longer reaches back to what the page shows, the page loads
 * again, unless a reply is being written. Every wording comes from the template.
 */

import { ackOf, messageOf } from './events.js';
import { follow } from './live-stream.js';
import { localizeTime } from './local-times.js';
import { ackAction, offersAck, showsImportance } from './message-marks.js';
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

  follow({
    config,
    signal,
    onEvent: (event) => {
      const message = messageOf(event, thread);
      if (message !== null) {
        append(message);
      }
      const acked = ackOf(event, thread);
      if (acked !== null) {
        markAcked(acked);
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
    onResync: () => {
      const draft = find(root, 'textarea[name="body"]', HTMLTextAreaElement);
      if (draft === null || draft.value.trim() === '') {
        window.location.reload();
      }
    },
  });

  return () => {
    controller.abort();
    mounted.delete(root);
  };
}
