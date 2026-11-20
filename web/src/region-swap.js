// @ts-check
/**
 * Swaps a region of the page for the one of an answer while keeping the person's place: the new
 * region keeps the scroll position of the old one, and the focus that was inside the old one goes
 * back to the element of the new one that stands for it.
 */

import { focusIndex } from './focus-return.js';
import { swapRegion } from './regions.js';

/**
 * The id and `href` an element is recognised by.
 * @param {Element} element
 * @returns {{ id: string, href: string }}
 */
function identity(element) {
  return { id: element.id, href: element.getAttribute('href') ?? '' };
}

/**
 * Swaps `current` for `incoming`, keeping its scroll position and the focus that was inside it.
 * @param {Element} current
 * @param {Element} incoming
 * @returns {Element} the region now in the page
 */
export function swapKeepingPlace(current, incoming) {
  const active = current.ownerDocument.activeElement;
  const was = active !== null && active !== current && current.contains(active) ? identity(active) : null;
  const { scrollTop, scrollLeft } = current;
  const next = swapRegion(current, incoming);
  next.scrollTop = scrollTop;
  next.scrollLeft = scrollLeft;
  if (was !== null) {
    const candidates = Array.from(next.querySelectorAll('[id], a[href]'));
    const target = candidates[focusIndex(was, candidates.map(identity))];
    if (target instanceof HTMLElement) {
      target.focus({ preventScroll: true });
    }
  }
  return next;
}
