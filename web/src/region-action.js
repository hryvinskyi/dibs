// @ts-check
/**
 * What a refresh does with a region of the screen: a region its own script keeps current is left
 * as it is, one the person is using a form in is held, and any other is swapped; and whether a
 * select's choice differs from the one it was rendered with, which marks a region in use. Pure,
 * over what the page reports, so a test reaches it without a page.
 */

/**
 * `swap` the region in; `skip` it, because its own script keeps it current; `hold` it, because
 * the person is using it.
 * @typedef {'swap' | 'skip' | 'hold'} RegionAction
 */

/**
 * What to do with a region when a refresh answers.
 * @param {{ ownLive: boolean, focusInside: boolean, edited: boolean }} region `ownLive`: it carries
 *   `data-live="own"`; `focusInside`: a form control in it has the focus; `edited`: a control in it
 *   holds a value other than its default
 * @returns {RegionAction}
 */
export function regionAction({ ownLive, focusInside, edited }) {
  if (ownLive) {
    return 'skip';
  }
  return focusInside || edited ? 'hold' : 'swap';
}

/**
 * A select as far as telling whether its choice changed: whether it lists its options as a box
 * (`multiple`, or a `size` above 1), the option chosen, and each option's state now and as
 * rendered.
 * @typedef {object} SelectState
 * @property {boolean} multiple
 * @property {number} size
 * @property {number} selectedIndex
 * @property {readonly { selected: boolean, defaultSelected: boolean }[]} options
 */

/**
 * Whether a select's chosen options differ from those it was rendered with. A drop-down with no
 * option marked as chosen was rendered with its first option chosen, and one with several marked
 * with the last of them.
 * @param {SelectState} select
 * @returns {boolean}
 */
export function selectChanged({ multiple, size, selectedIndex, options }) {
  if (multiple || size > 1) {
    return options.some((option) => option.selected !== option.defaultSelected);
  }
  const marked = options.map((option) => option.defaultSelected).lastIndexOf(true);
  const rendered = marked === -1 && options.length > 0 ? 0 : marked;
  return selectedIndex !== rendered;
}
