// @ts-check
/**
 * Events of the project's log as the event stream sends them: a frame's data parsed into a typed
 * event, whether an event announces a message of one thread, an acknowledgement in it, or a change
 * a page shows, and a bounded memory of the event ids already handled.
 */

/** A frame whose data does not have the shape the stream promises. */
export class ContractError extends Error {
  /**
   * @param {string} path where in the data the shape broke
   * @param {string} expected what was expected there
   */
  constructor(path, expected) {
    super(`${path}: expected ${expected}`);
    this.name = 'ContractError';
    /** @readonly */
    this.path = path;
  }
}

/**
 * How much a message asks for its readers' attention.
 * @typedef {'normal' | 'high' | 'urgent'} Importance
 */

/**
 * A message the stream carries with the event that announces it.
 * @typedef {object} StreamMessage
 * @property {number} id
 * @property {number} thread
 * @property {string} author the author's handle, or `dibs` for what dibs wrote
 * @property {boolean} system whether dibs wrote it, in the project's name
 * @property {string} label the label of the session it was written from
 * @property {string} body plain text, never markup
 * @property {Importance} importance
 * @property {boolean} ackRequired whether each recipient is asked to acknowledge it
 * @property {string} createdAt an RFC 3339 time
 */

/** @type {ReadonlyMap<string, Importance>} */
const IMPORTANCES = new Map([
  ['normal', 'normal'],
  ['high', 'high'],
  ['urgent', 'urgent'],
]);

/**
 * A recipient's acknowledgement of a message, as an `acked` event tells it.
 * @typedef {object} StreamAck
 * @property {number} thread
 * @property {number} message
 * @property {string} member the handle of the recipient who acknowledged it
 * @property {string} at an RFC 3339 time
 */

/** The kind of the event that tells an acknowledgement. */
const ACKED = 'acked';

/**
 * One event of the log.
 * @typedef {object} StreamEvent
 * @property {number} id its position in the log
 * @property {string} kind its stable name, such as `claimed` or `message_posted`
 * @property {StreamMessage | null} message the message it announces, when it announces one
 * @property {StreamAck | null} ack the acknowledgement it tells, for an `acked` event
 */

/**
 * @param {unknown} value
 * @returns {value is Record<string, unknown>}
 */
const isRecord = (value) => typeof value === 'object' && value !== null && !Array.isArray(value);

/**
 * @param {unknown} value
 * @param {string} path
 * @returns {number}
 */
const id = (value, path) => {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 1) {
    throw new ContractError(path, 'a positive integer');
  }
  return value;
};

/**
 * @param {unknown} value
 * @param {string} path
 * @returns {string}
 */
const text = (value, path) => {
  if (typeof value !== 'string') {
    throw new ContractError(path, 'a string');
  }
  return value;
};

/**
 * @param {unknown} value
 * @param {string} path
 * @returns {Importance}
 */
const importance = (value, path) => {
  const known = typeof value === 'string' ? IMPORTANCES.get(value) : undefined;
  if (known === undefined) {
    throw new ContractError(path, 'normal, high or urgent');
  }
  return known;
};

/**
 * @param {unknown} value
 * @param {string} path
 * @returns {boolean}
 */
const flag = (value, path) => {
  if (typeof value !== 'boolean') {
    throw new ContractError(path, 'true or false');
  }
  return value;
};

/**
 * @param {unknown} raw
 * @returns {StreamMessage}
 */
function parseMessage(raw) {
  if (!isRecord(raw)) {
    throw new ContractError('$.message', 'an object');
  }
  return {
    id: id(raw.id, '$.message.id'),
    thread: id(raw.thread, '$.message.thread'),
    author: text(raw.author, '$.message.author'),
    system: flag(raw.system, '$.message.system'),
    label: text(raw.label, '$.message.label'),
    body: text(raw.body, '$.message.body'),
    importance: importance(raw.importance, '$.message.importance'),
    ackRequired: flag(raw.ack_required, '$.message.ack_required'),
    createdAt: text(raw.created_at, '$.message.created_at'),
  };
}

/**
 * The acknowledgement an `acked` event tells: its payload's ids, the recipient's handle from the
 * event's `members`, and the event's time.
 * @param {Record<string, unknown>} raw the event
 * @returns {StreamAck}
 */
function parseAck(raw) {
  const payload = raw.payload;
  if (!isRecord(payload)) {
    throw new ContractError('$.payload', 'an object');
  }
  const by = id(payload.by, '$.payload.by');
  const members = raw.members;
  if (!isRecord(members)) {
    throw new ContractError('$.members', 'an object');
  }
  return {
    thread: id(payload.thread, '$.payload.thread'),
    message: id(payload.message, '$.payload.message'),
    member: text(members[String(by)], `$.members.${by}`),
    at: text(raw.at, '$.at'),
  };
}

/**
 * Parses the data of one server-sent event.
 * @param {unknown} data the `data` of a `MessageEvent`: JSON text
 * @returns {StreamEvent}
 * @throws {ContractError} when the data is not JSON of the event's shape
 */
export function parseEvent(data) {
  if (typeof data !== 'string') {
    throw new ContractError('$', 'JSON text');
  }
  /** @type {unknown} */
  let raw;
  try {
    raw = JSON.parse(data);
  } catch {
    throw new ContractError('$', 'JSON text');
  }
  if (!isRecord(raw)) {
    throw new ContractError('$', 'an object');
  }
  const kind = text(raw.kind, '$.kind');
  if (kind === '') {
    throw new ContractError('$.kind', 'a name');
  }
  return {
    id: id(raw.id, '$.id'),
    kind,
    message: raw.message === undefined || raw.message === null ? null : parseMessage(raw.message),
    ack: kind === ACKED ? parseAck(raw) : null,
  };
}

/**
 * The message an event announces, when it is a message of this thread.
 * @param {StreamEvent} event
 * @param {number} thread
 * @returns {StreamMessage | null}
 */
export function messageOf(event, thread) {
  return event.kind === 'message_posted' && event.message !== null && event.message.thread === thread
    ? event.message
    : null;
}

/**
 * The acknowledgement an event tells, when it is one in this thread.
 * @param {StreamEvent} event
 * @param {number} thread
 * @returns {StreamAck | null}
 */
export function ackOf(event, thread) {
  return event.ack !== null && event.ack.thread === thread ? event.ack : null;
}

/**
 * Whether an event is of one of these kinds.
 * @param {StreamEvent} event
 * @param {ReadonlySet<string>} kinds
 * @returns {boolean}
 */
export function isChange(event, kinds) {
  return kinds.has(event.kind);
}

/**
 * The event ids handled so far, at most `limit` of them; the oldest are forgotten first.
 * @param {number} limit
 * @returns {{ add(id: number): boolean, has(id: number): boolean }} `add` answers whether the id
 *   was new
 */
export function createSeen(limit) {
  /** @type {Set<number>} */
  const ids = new Set();
  const bound = Math.max(1, Math.floor(limit));
  return {
    add(eventId) {
      if (ids.has(eventId)) {
        return false;
      }
      ids.add(eventId);
      if (ids.size > bound) {
        const oldest = ids.values().next();
        if (!oldest.done) {
          ids.delete(oldest.value);
        }
      }
      return true;
    },
    has(eventId) {
      return ids.has(eventId);
    },
  };
}
