// @ts-check
/**
 * Which input moves the inspector: a click that is the browser's to handle or the page's, and a key
 * that closes the inspector. Pure, so a test reaches it without a page.
 */

/**
 * Whether a click is the primary button with no modifier key, not handled already.
 * @param {Pick<MouseEvent, 'button' | 'ctrlKey' | 'metaKey' | 'shiftKey' | 'altKey' | 'defaultPrevented'>} event
 * @returns {boolean}
 */
export function isPlainClick(event) {
  return (
    event.button === 0 &&
    !event.ctrlKey &&
    !event.metaKey &&
    !event.shiftKey &&
    !event.altKey &&
    !event.defaultPrevented
  );
}

/**
 * Whether a key closes the inspector: `Escape`, not handled already, not typed into a field, with
 * no dialog open and an object selected.
 * @param {{ key: string, inField: boolean, dialogOpen: boolean, selected: boolean, defaultPrevented: boolean }} press
 * @returns {boolean}
 */
export function closesOnKey({ key, inField, dialogOpen, selected, defaultPrevented }) {
  return key === 'Escape' && !defaultPrevented && !inField && !dialogOpen && selected;
}
