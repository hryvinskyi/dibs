// @ts-check
/**
 * What a live page tells its script through `data-*` attributes, parsed into typed values. The
 * attributes are strings the server printed; a missing or malformed one is a `ContractError`.
 */

import { ContractError } from './events.js';

/**
 * The attributes of an element, as `HTMLElement.dataset` holds them.
 * @typedef {Readonly<Record<string, string | undefined>>} DataAttributes
 */

/**
 * Where the stream of a page is read from, and from which event on.
 * @typedef {object} StreamConfig
 * @property {string} streamUrl
 * @property {string} sessionUrl the address that answers 401 once the page's session has ended
 * @property {number} after the last event the page already shows; 0 for none
 */

/**
 * `sender` is the handle of the member who started the thread, `''` for a thread dibs started;
 * `systemAuthor` is how a message dibs wrote names its author.
 * @typedef {StreamConfig & {
 *   thread: number,
 *   me: string,
 *   sender: string,
 *   systemAuthor: string,
 *   newMessageLabel: string,
 *   newMessagesLabel: string,
 *   ackAction: string,
 * }} ThreadConfig
 */

/** @typedef {StreamConfig & { kinds: ReadonlySet<string> }} StatusConfig */

/** @typedef {StatusConfig & { fragmentUrl: string }} BoardConfig */

/**
 * @param {DataAttributes} data
 * @param {string} name
 * @returns {string}
 */
function required(data, name) {
  const value = data[name];
  if (value === undefined || value.trim() === '') {
    throw new ContractError(`dataset.${name}`, 'a value');
  }
  return value;
}

/**
 * @param {DataAttributes} data
 * @param {string} name
 * @param {number} min
 * @returns {number}
 */
function integer(data, name, min) {
  const value = required(data, name).trim();
  const number = /^\d+$/.test(value) ? Number(value) : Number.NaN;
  if (!Number.isSafeInteger(number) || number < min) {
    throw new ContractError(`dataset.${name}`, `a whole number from ${min}`);
  }
  return number;
}

/**
 * @param {DataAttributes} data
 * @returns {StreamConfig}
 */
function streamConfig(data) {
  return {
    streamUrl: required(data, 'streamUrl'),
    sessionUrl: required(data, 'sessionUrl'),
    after: integer(data, 'after', 0),
  };
}

/**
 * The configuration of a thread page.
 * @param {DataAttributes} data
 * @returns {ThreadConfig}
 * @throws {ContractError}
 */
export function parseThreadConfig(data) {
  return {
    ...streamConfig(data),
    thread: integer(data, 'threadId', 1),
    me: required(data, 'me'),
    sender: (data.sender ?? '').trim(),
    systemAuthor: required(data, 'systemAuthor'),
    newMessageLabel: required(data, 'newMessageLabel'),
    newMessagesLabel: required(data, 'newMessagesLabel'),
    ackAction: placeholder(data, 'ackAction', '{message}'),
  };
}

/**
 * A required attribute that holds `mark` where a value is to be put in.
 * @param {DataAttributes} data
 * @param {string} name
 * @param {string} mark
 * @returns {string}
 */
function placeholder(data, name, mark) {
  const value = required(data, name);
  if (!value.includes(mark)) {
    throw new ContractError(`dataset.${name}`, `a value holding ${mark}`);
  }
  return value;
}

/**
 * The configuration of a page that offers a reload when the claims change.
 * @param {DataAttributes} data
 * @returns {StatusConfig}
 * @throws {ContractError}
 */
export function parseStatusConfig(data) {
  const kinds = new Set(required(data, 'changedKinds').split(/\s+/).filter((kind) => kind !== ''));
  return { ...streamConfig(data), kinds };
}

/**
 * Whether a value is a path of this service that a browser cannot read as another site's: it
 * starts with one `/`, holds no `\\` (which browsers read as `/`), and only visible ASCII.
 * @param {string} value
 * @returns {boolean}
 */
export function isLocalPath(value) {
  return value.startsWith('/') && !value.startsWith('//') && !value.includes('\\') && /^[\x21-\x7e]+$/.test(value);
}

/**
 * The configuration of the board: a page that renders its columns again when the claims or the
 * items change, from a path of this service.
 * @param {DataAttributes} data
 * @returns {BoardConfig}
 * @throws {ContractError}
 */
export function parseBoardConfig(data) {
  const fragmentUrl = required(data, 'fragmentUrl');
  if (!isLocalPath(fragmentUrl)) {
    throw new ContractError('dataset.fragmentUrl', 'a path of this service');
  }
  return { ...parseStatusConfig(data), fragmentUrl };
}

/**
 * The label for a number of new messages: `one` for a single one, otherwise `many` with every
 * `{count}` replaced by the number.
 * @param {number} count
 * @param {string} one
 * @param {string} many
 * @returns {string}
 */
export function countLabel(count, one, many) {
  return count === 1 ? one : many.replaceAll('{count}', String(count));
}
