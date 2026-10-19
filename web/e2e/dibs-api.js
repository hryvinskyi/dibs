// @ts-check
/**
 * The server's JSON API as the cases use it: start a conversation and post a message as a member.
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
 * @param {string} baseUrl
 * @param {string} token a member's bearer token
 * @param {string} path
 * @param {unknown} payload
 * @returns {Promise<unknown>} the parsed body of a `201`
 */
async function post(baseUrl, token, path, payload) {
  const response = await fetch(`${baseUrl}${path}`, {
    method: 'POST',
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
    body: JSON.stringify(payload),
    signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  });
  if (response.status !== 201) {
    throw new Error(`POST ${path} answered ${response.status}`);
  }
  return response.json();
}

/**
 * Starts a conversation with the members `to`.
 * @param {string} baseUrl
 * @param {string} token the author's token
 * @param {{ subject: string, to: string[], body: string }} thread
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
 * @returns {Promise<number>} the message's id
 */
export async function postMessage(baseUrl, token, threadId, text) {
  const body = await post(baseUrl, token, `/v1/threads/${threadId}/messages`, {
    label: 'e2e/api',
    body: text,
  });
  return idUnder(body, 'message');
}
