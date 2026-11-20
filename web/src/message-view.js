// @ts-check
/**
 * What a conversation shows for a message or an acknowledgement that arrives live, decided apart
 * from the page: who it names, which marks it carries, where its acknowledge form posts and the
 * text of its time in the viewer's zone at the moment given. Pure, so a test reaches it without a
 * page and with a fixed clock.
 */

import { clockText } from './clock-text.js';
import { anchorOf } from './message-anchors.js';
import { ackAction, offersAck, showsImportance } from './message-marks.js';

/**
 * A moment as a `<time>` shows it: its `datetime`, and its wall-clock text in the viewer's zone, or
 * `null` when the moment is not a time and the element keeps the text it has.
 * @typedef {{ datetime: string, text: string | null }} Stamp
 */

/**
 * What a message appended live shows.
 * @typedef {object} MessageView
 * @property {string} anchor its element's `id`
 * @property {boolean} mine whether the person reading the page wrote it
 * @property {boolean} system whether dibs wrote it
 * @property {string} author the name it is shown under
 * @property {string | null} session the label of the session it was written from; `null` for what
 *   dibs wrote, which names no session
 * @property {string} body plain text
 * @property {import('./events.js').Importance | null} badge the raised importance it is shown with
 * @property {boolean} asksAck whether it asks its recipients to acknowledge it
 * @property {string | null} ackAction where the person's acknowledge form posts; `null` when they
 *   are not offered one
 * @property {Stamp} time when it was written
 */

/**
 * What an acknowledgement that arrives live shows in its message's list.
 * @typedef {object} AckEntryView
 * @property {string} member the handle of the recipient who acknowledged
 * @property {boolean} you whether that is the person reading the page
 * @property {Stamp} time when they acknowledged
 */

/**
 * What a page tells a conversation about who reads it and how to word what arrives.
 * @typedef {Pick<import('./view-config.js').ThreadConfig, 'me' | 'sender' | 'systemAuthor' | 'ackAction'>} ViewerConfig
 */

/**
 * How a moment written as an RFC 3339 time shows at `now`.
 * @param {string} datetime
 * @param {Date} now
 * @returns {Stamp}
 */
export function stampOf(datetime, now) {
  const at = new Date(datetime);
  return { datetime, text: Number.isNaN(at.getTime()) ? null : clockText(at, now) };
}

/**
 * What a message appended live shows to the person reading the page at `now`.
 * @param {import('./events.js').StreamMessage} message
 * @param {ViewerConfig} config
 * @param {Date} now
 * @returns {MessageView}
 */
export function messageView(message, config, now) {
  return {
    anchor: anchorOf(message.id),
    mine: !message.system && message.author === config.me,
    system: message.system,
    author: message.system ? config.systemAuthor : message.author,
    session: message.system ? null : message.label,
    body: message.body,
    badge: showsImportance(message.importance) ? message.importance : null,
    asksAck: message.ackRequired,
    ackAction: offersAck(message, config.me, config.sender) ? ackAction(config.ackAction, message.id) : null,
    time: stampOf(message.createdAt, now),
  };
}

/**
 * What an acknowledgement that arrives live shows to the person `me` at `now`.
 * @param {import('./events.js').StreamAck} acked
 * @param {string} me
 * @param {Date} now
 * @returns {AckEntryView}
 */
export function ackEntryView(acked, me, now) {
  return { member: acked.member, you: acked.member === me, time: stampOf(acked.at, now) };
}
