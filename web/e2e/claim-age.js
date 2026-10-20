// @ts-check
/**
 * A claim made to look as if its holder went quiet some time ago, for a case that needs a stale
 * claim without waiting for the project's stale threshold: the claim's times in the server's
 * database are moved back. The server reads every claim from the database on each request, so the
 * change shows on the next page it renders.
 */

import { DatabaseSync } from 'node:sqlite';

/** How long a write may wait for the server's own transactions, in milliseconds. */
const BUSY_TIMEOUT_MS = 5000;

/** Nanoseconds in an hour: the store keeps times as nanoseconds since 1970. */
const HOUR_NS = 3_600_000_000_000n;

/**
 * Moves the times of the claim on `itemId` back by `hours`.
 * @param {string} database the server's database file
 * @param {number} itemId
 * @param {number} hours
 * @returns {void}
 */
export function ageClaim(database, itemId, hours) {
  const db = new DatabaseSync(database, { timeout: BUSY_TIMEOUT_MS });
  try {
    const shift = BigInt(Math.round(hours)) * HOUR_NS;
    const changed = db
      .prepare(
        'UPDATE claims SET claimed_at = claimed_at - ?, renewed_at = renewed_at - ?, last_seen = last_seen - ? WHERE item_id = ?',
      )
      .run(shift, shift, shift, itemId);
    if (Number(changed.changes) !== 1) {
      throw new Error(`no claim on item ${itemId} to age`);
    }
  } finally {
    db.close();
  }
}
