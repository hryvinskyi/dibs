// @ts-check
/**
 * What the search palette offers for the query in its box. Hits belong to the query that was asked
 * for them: an answer that cannot be shown takes the earlier query's hits away rather than leaving
 * them to be opened, and while a newer query is still being asked nothing is offered to act on.
 */

/**
 * What the palette does with the answer to its query.
 * @typedef {{ kind: 'signed-out' } | { kind: 'clear' } | { kind: 'show', region: Element }} Offer
 */

/**
 * What to do with the answer to the current query: end the session, show the results region, or
 * clear what is listed, since an answer that is not the region cannot stand in for its hits.
 * @param {import('./regions.js').RegionsAnswer} answer
 * @returns {Offer}
 */
export function offerFor(answer) {
  if (answer.kind === 'signed-out') {
    return { kind: 'signed-out' };
  }
  if (answer.kind === 'failed') {
    return { kind: 'clear' };
  }
  const region = answer.regions.get('results');
  return region === undefined ? { kind: 'clear' } : { kind: 'show', region };
}

/**
 * How many hits a key can act on: the hits listed once they answer the query in the box, none while
 * a newer query is waiting or being asked.
 * @param {number} listed how many hits are listed
 * @param {boolean} asking whether the query in the box has not been answered yet
 * @returns {number}
 */
export function offered(listed, asking) {
  return asking ? 0 : listed;
}
