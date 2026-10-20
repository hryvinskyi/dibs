// @ts-check
/**
 * What a live thread page marks on a message that arrives: whether its importance gets a badge,
 * whether the person is offered the acknowledge button, and where that button posts. Pure, so a
 * test reaches it without a page; the server decides whether an acknowledgement is accepted.
 */

/**
 * Whether a message's importance is shown with a badge: only when it is raised above normal.
 * @param {import('./events.js').Importance} importance
 * @returns {boolean}
 */
export function showsImportance(importance) {
  return importance !== 'normal';
}

/**
 * The handle of the member a message speaks for: its author, or, for what dibs wrote, the member
 * who started the thread, on whose behalf dibs wrote it — `''` when dibs started the thread too.
 * @param {Pick<import('./events.js').StreamMessage, 'author' | 'system'>} message
 * @param {string} sender the handle of the member who started the thread, `''` for dibs
 * @returns {string}
 */
export function speaksFor(message, sender) {
  return message.system ? sender : message.author;
}

/**
 * Whether the person reading the page is offered the acknowledge button for a message: it asks
 * for an acknowledgement and does not speak for them.
 * @param {Pick<import('./events.js').StreamMessage, 'ackRequired' | 'author' | 'system'>} message
 * @param {string} me the handle of the person reading the page
 * @param {string} sender the handle of the member who started the thread, `''` for dibs
 * @returns {boolean}
 */
export function offersAck(message, me, sender) {
  return message.ackRequired && speaksFor(message, sender) !== me;
}

/**
 * The address the acknowledge form posts to: the page's template with every `{message}` replaced
 * by the message's id.
 * @param {string} template
 * @param {number} message
 * @returns {string}
 */
export function ackAction(template, message) {
  return template.replaceAll('{message}', String(message));
}
