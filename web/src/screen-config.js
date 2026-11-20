// @ts-check
/**
 * The live configuration of a screen, read from the attributes of its `.app` element: the
 * addresses of the stream and of the session check, the last event the page shows, and the kinds
 * of change the screen refreshes on. Pure, so a test reaches it without a page.
 */

import { ContractError } from './events.js';
import { isLocalPath } from './view-config.js';

/**
 * The screen's stream, and the kinds of change it refreshes on (`null`: every change).
 * @typedef {{ stream: import('./view-config.js').StreamConfig, kinds: ReadonlySet<string> | null }} ScreenConfig
 */

/**
 * A path of this service an attribute names.
 * @param {import('./view-config.js').DataAttributes} data
 * @param {string} name
 * @returns {string}
 * @throws {ContractError}
 */
function localPath(data, name) {
  const value = data[name] ?? '';
  if (!isLocalPath(value)) {
    throw new ContractError(`dataset.${name}`, 'a path of this service');
  }
  return value;
}

/**
 * The configuration of a screen, from the attributes of its `.app` element: the stream's and the
 * session check's addresses, the last event the page shows (`data-after`), and the change kinds
 * (`data-changed-kinds`, separated by white space; absent: every change).
 * @param {import('./view-config.js').DataAttributes} data
 * @returns {ScreenConfig}
 * @throws {ContractError}
 */
export function parseScreenConfig(data) {
  const after = (data.after ?? '').trim();
  const cursor = /^\d+$/.test(after) ? Number(after) : Number.NaN;
  if (!Number.isSafeInteger(cursor)) {
    throw new ContractError('dataset.after', 'a whole number from 0');
  }
  const listed = data.changedKinds;
  return {
    stream: { streamUrl: localPath(data, 'streamUrl'), sessionUrl: localPath(data, 'sessionUrl'), after: cursor },
    kinds: listed === undefined ? null : new Set(listed.split(/\s+/).filter((kind) => kind !== '')),
  };
}
