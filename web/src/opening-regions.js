// @ts-check
/**
 * What an in-place opening shows of the answer: the address opened is asked for every region of
 * the screen the page has, because opening an object can change what the other regions show (a
 * conversation opened is read, so the counts and the mark that showed it unread change with it).
 * The inspector, which the person asked for, is always opened from the answer; another region is
 * refreshed from it, where it was scrolled to, unless the person is using it, so that nothing they
 * typed is lost.
 */

/** The regions of a screen, in the order an opening swaps them in. */
const SCREEN_REGIONS = Object.freeze(['rail', 'list', 'inspector']);

/** The region an opening is for. */
export const OPENED_REGION = 'inspector';

/**
 * `open` the region of the answer as the one the person asked for; `refresh` the region from the
 * answer, keeping where it was scrolled to; `keep` the one the page shows.
 * @typedef {'open' | 'refresh' | 'keep'} OpeningAction
 */

/**
 * The regions an opening asks for: those of the screen's regions the page has.
 * @param {readonly string[]} present the names of the regions on the page
 * @returns {string[]}
 */
export function regionsToAsk(present) {
  return SCREEN_REGIONS.filter((name) => present.includes(name));
}

/**
 * What an opening does with one region of its answer.
 * @param {string} name the region's name
 * @param {boolean} inUse whether the person is using the region the page shows: a control in it
 *   holds a value other than the one it was rendered with
 * @returns {OpeningAction}
 */
export function openingAction(name, inUse) {
  if (name === OPENED_REGION) {
    return 'open';
  }
  return inUse ? 'keep' : 'refresh';
}
