// @ts-check
/**
 * Where the focus goes back to once a region it was inside has been replaced: the element of the
 * new region that stands for the one that had it. Pure, so a test reaches it without a page.
 */

/**
 * The candidate with the id of the element that had the focus, or else the one with its `href`;
 * -1 when none has. An empty id or `href` matches nothing.
 * @param {{ id: string, href: string }} was
 * @param {readonly { id: string, href: string }[]} candidates
 * @returns {number}
 */
export function focusIndex(was, candidates) {
  const byId = was.id === '' ? -1 : candidates.findIndex((candidate) => candidate.id === was.id);
  if (byId !== -1 || was.href === '') {
    return byId;
  }
  return candidates.findIndex((candidate) => candidate.href === was.href);
}
