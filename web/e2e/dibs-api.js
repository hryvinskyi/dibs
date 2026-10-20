// @ts-check
/**
 * The server's JSON API as the cases use it: start a conversation and post a message as a member,
 * each optionally raised in importance or asking to be acknowledged; acknowledge a message; import
 * items as an integration does; open a session and claim an item from it.
 * Answers are parsed into the few values a case needs; a body of another shape is an error.
 */

/** How long a request may take, in milliseconds. */
const REQUEST_TIMEOUT_MS = 10_000;

/**
 * @param {unknown} value
 * @returns {value is Record<string, unknown>}
 */
function isRecord(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * The positive whole number at `body[key].id`.
 * @param {unknown} body
 * @param {string} key
 * @returns {number}
 */
function idUnder(body, key) {
  const inner = isRecord(body) ? body[key] : undefined;
  const id = isRecord(inner) ? inner.id : undefined;
  if (typeof id !== 'number' || !Number.isInteger(id) || id <= 0) {
    throw new Error(`the answer holds no ${key} id`);
  }
  return id;
}

/**
 * The positive whole number at `body[key]`.
 * @param {unknown} body
 * @param {string} key
 * @returns {number}
 */
function idAt(body, key) {
  const id = isRecord(body) ? body[key] : undefined;
  if (typeof id !== 'number' || !Number.isInteger(id) || id <= 0) {
    throw new Error(`the answer holds no ${key}`);
  }
  return id;
}

/**
 * @param {string} baseUrl
 * @param {string} token a member's bearer token
 * @param {'POST' | 'PUT'} method
 * @param {string} path
 * @param {unknown} payload
 * @param {number} expected the status a success answers
 * @returns {Promise<unknown>} the parsed body
 */
async function send(baseUrl, token, method, path, payload, expected) {
  const response = await fetch(`${baseUrl}${path}`, {
    method,
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
    body: JSON.stringify(payload),
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  });
  if (response.status !== expected) {
    throw new Error(`${method} ${path} answered ${response.status}`);
  }
  return response.json();
}

/**
 * @param {string} baseUrl
 * @param {string} token a member's bearer token
 * @param {string} path
 * @param {unknown} payload
 * @returns {Promise<unknown>} the parsed body of a `201`
 */
function post(baseUrl, token, path, payload) {
  return send(baseUrl, token, 'POST', path, payload, 201);
}

/**
 * What a message asks of the people it reaches; an ordinary message when left out.
 * @typedef {{ importance?: 'normal' | 'high' | 'urgent', ack_required?: boolean }} Asking
 */

/**
 * Starts a conversation with the members `to`.
 * @param {string} baseUrl
 * @param {string} token the author's token
 * @param {{ subject: string, to: string[], body: string } & Asking} thread
 * @returns {Promise<number>} the conversation's id
 */
export async function startThread(baseUrl, token, thread) {
  const body = await post(baseUrl, token, '/v1/threads', { ...thread, label: 'e2e/api' });
  return idUnder(body, 'thread');
}

/**
 * Posts a message into a conversation.
 * @param {string} baseUrl
 * @param {string} token the author's token
 * @param {number} threadId
 * @param {string} text
 * @param {Asking} [asking]
 * @returns {Promise<number>} the message's id
 */
export async function postMessage(baseUrl, token, threadId, text, asking = {}) {
  const body = await post(baseUrl, token, `/v1/threads/${threadId}/messages`, {
    ...asking,
    label: 'e2e/api',
    body: text,
  });
  return idUnder(body, 'message');
}

/**
 * Acknowledges a message as one of its recipients.
 * @param {string} baseUrl
 * @param {string} token the recipient's token
 * @param {number} messageId
 * @returns {Promise<void>}
 */
export async function ackMessage(baseUrl, token, messageId) {
  await post(baseUrl, token, `/v1/messages/${messageId}/ack`, {});
}

/**
 * Imports items as an integration does, from the source `board`.
 * @param {string} baseUrl
 * @param {string} token an admin's or an importer's token
 * @param {{ ref: string, title: string, priority?: number, areas_hint?: string[] }[]} items
 * @returns {Promise<number[]>} the items' ids, in the order given
 */
export async function importItems(baseUrl, token, items) {
  const body = await send(baseUrl, token, 'PUT', '/v1/items', { source: 'board', items }, 200);
  const outcomes = isRecord(body) && Array.isArray(body.items) ? body.items : [];
  if (outcomes.length !== items.length) {
    throw new Error('the import answered another number of items');
  }
  return outcomes.map((/** @type {unknown} */ outcome) => idAt(outcome, 'id'));
}

/**
 * Registers a session of a member on a machine.
 * @param {string} baseUrl
 * @param {string} token the member's token
 * @param {string} host
 * @returns {Promise<number>} the session's id
 */
export async function openSession(baseUrl, token, host) {
  const body = await send(baseUrl, token, 'POST', '/v1/sessions', { host, label: 'e2e/api', agent: 'e2e' }, 200);
  return idAt(body, 'session_id');
}

/**
 * Claims an item from a session.
 * @param {string} baseUrl
 * @param {string} token the member's token
 * @param {number} itemId
 * @param {number} sessionId
 * @returns {Promise<void>}
 */
export async function claimItem(baseUrl, token, itemId, sessionId) {
  await send(baseUrl, token, 'POST', `/v1/items/${itemId}/claim`, { session_id: sessionId, areas: [] }, 200);
}
