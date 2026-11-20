// @ts-check
/**
 * The elements of a conversation built from what arrives live: a message cloned from the page's
 * message `<template>` and an acknowledgement cloned from its acknowledgement `<template>`, both
 * filled with `textContent` from a view decided apart from the page. Every wording comes from the
 * template.
 */

import { anchorOf } from './message-anchors.js';

/**
 * Writes a stamp into a `<time>`: its `datetime`, and its text when the stamp has one.
 * @param {Element | null} time
 * @param {import('./message-view.js').Stamp} stamp
 * @returns {void}
 */
function writeStamp(time, stamp) {
  if (time === null) {
    return;
  }
  time.setAttribute('datetime', stamp.datetime);
  if (stamp.text !== null) {
    time.textContent = stamp.text;
  }
}

/**
 * The first element of a template, cloned.
 * @param {HTMLTemplateElement} template
 * @returns {HTMLElement | null}
 */
function cloneOf(template) {
  const item = template.content.firstElementChild?.cloneNode(true);
  return item instanceof HTMLElement ? item : null;
}

/**
 * The element of a message appended live; `null` when the template holds no element.
 * @param {HTMLTemplateElement} template
 * @param {import('./message-view.js').MessageView} view
 * @returns {HTMLElement | null}
 */
export function buildMessage(template, view) {
  const item = cloneOf(template);
  if (item === null) {
    return null;
  }
  item.id = view.anchor;
  item.classList.toggle('msg--mine', view.mine);
  item.classList.toggle('msg--system', view.system);
  const fill = (/** @type {string} */ field, /** @type {string} */ value) => {
    const target = item.querySelector(`[data-field="${field}"]`);
    if (target !== null) {
      target.textContent = value;
    }
  };
  fill('author', view.author);
  if (view.session === null) {
    item.querySelector('[data-field="session"]')?.remove();
  } else {
    fill('session', view.session);
  }
  fill('body', view.body);
  if (view.badge !== null) {
    const badge = item.querySelector(`[data-importance="${view.badge}"]`);
    if (badge instanceof HTMLElement) {
      badge.hidden = false;
    }
  }
  const ack = item.querySelector('[data-ack]');
  if (view.asksAck && ack instanceof HTMLElement) {
    ack.hidden = false;
  }
  const ackForm = item.querySelector('form[data-ack-form]');
  if (view.ackAction !== null && ackForm instanceof HTMLFormElement) {
    ackForm.action = view.ackAction;
    ackForm.hidden = false;
  }
  writeStamp(item.querySelector('time[data-field="time"]'), view.time);
  return item;
}

/**
 * Marks a recipient as having acknowledged a message of the conversation under `root`: their entry
 * in the message's list is added or replaced, and once the person reading the page acknowledged,
 * their own acknowledge button goes away. A message the conversation does not show is left alone.
 * @param {HTMLElement} root the conversation
 * @param {HTMLTemplateElement} template the acknowledgement's template
 * @param {number} message
 * @param {import('./message-view.js').AckEntryView} view
 * @returns {void}
 */
export function showAck(root, template, message, view) {
  const item = root.ownerDocument.getElementById(anchorOf(message));
  const acks = item !== null && root.contains(item) ? item.querySelector('[data-ack-list]') : null;
  const entry = cloneOf(template);
  if (item === null || acks === null || entry === null) {
    return;
  }
  entry.dataset.ackMember = view.member;
  const name = entry.querySelector('[data-field="name"]');
  if (name !== null) {
    name.textContent = view.member;
  }
  const you = entry.querySelector('[data-field="you"]');
  if (you instanceof HTMLElement) {
    if (view.you) {
      you.hidden = false;
    } else {
      you.remove();
    }
  }
  writeStamp(entry.querySelector('time[data-field="time"]'), view.time);
  const listed = Array.from(acks.querySelectorAll('[data-ack-member]')).find(
    (element) => element instanceof HTMLElement && element.dataset.ackMember === view.member,
  );
  if (listed === undefined) {
    acks.append(entry);
  } else {
    listed.replaceWith(entry);
  }
  if (view.you) {
    item.querySelectorAll('form[data-ack-form]').forEach((form) => form.remove());
  }
}
