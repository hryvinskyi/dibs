// @ts-check
/**
 * Applies the regions of an answer to the page, region by region: a region its own script keeps
 * current is left as it is, one the person is using a form in is held, and any other is swapped.
 * Once the person leaves the held regions without a change in any of them, they are released and
 * the page is told, so it can ask for them again.
 */

import { focusInForm, hasEdits, isFormControl } from './form-use.js';
import { findRegion } from './regions.js';
import { regionAction } from './region-action.js';

/**
 * @typedef {object} ApplyDeps
 * @property {(current: Element, incoming: Element) => void} swap swaps one region in
 * @property {() => void} onReleased called when the person has left the held regions, with no
 *   change in any of them
 * @property {AbortSignal} signal ends the watch on the held regions
 */

/**
 * @typedef {object} RegionApplier
 * @property {(regions: ReadonlyMap<string, Element>, names: readonly string[]) => boolean} apply
 *   applies the regions `names` of an answer, answering whether it held any
 */

/**
 * An applier of answers to the regions inside `app`.
 * @param {Element} app the `.app` element
 * @param {ApplyDeps} deps
 * @returns {RegionApplier}
 */
export function regionApplier(app, { swap, onReleased, signal }) {
  /** The regions the last answer held, because the person was using them. @type {Set<string>} */
  let held = new Set();

  /**
   * Whether the person is still using one of the held regions as the focus moves to `next`.
   * @param {Element | null} next
   * @returns {boolean}
   */
  const stillInUse = (next) => [...held].some((name) => {
    const region = findRegion(app, name);
    return region !== null && (hasEdits(region) || (next !== null && region.contains(next) && isFormControl(next)));
  });

  app.addEventListener('focusout', (event) => {
    if (held.size === 0) {
      return;
    }
    const next = event instanceof FocusEvent && event.relatedTarget instanceof Element ? event.relatedTarget : null;
    if (!stillInUse(next)) {
      held = new Set();
      onReleased();
    }
  }, { signal });

  return {
    apply(regions, names) {
      /** @type {Set<string>} */
      const holding = new Set();
      /** @type {{ [A in import('./region-action.js').RegionAction]: (current: Element, incoming: Element, name: string) => void }} */
      const act = {
        swap: (current, incoming) => swap(current, incoming),
        skip: () => {},
        hold: (_current, _incoming, name) => {
          holding.add(name);
        },
      };
      for (const name of names) {
        const current = findRegion(app, name);
        const incoming = regions.get(name);
        if (current === null || incoming === undefined) {
          continue;
        }
        const action = regionAction({
          ownLive: current.getAttribute('data-live') === 'own',
          focusInside: focusInForm(current),
          edited: hasEdits(current),
        });
        act[action](current, incoming, name);
      }
      held = holding;
      return held.size > 0;
    },
  };
}
