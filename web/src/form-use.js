// @ts-check
/**
 * Reads off the page whether the person is using a form in a region: the focus on a form control
 * inside it, or a control inside it holding a value other than the one it was rendered with.
 */

import { selectChanged } from './region-action.js';

/**
 * Whether an element is a form control, or inside a form.
 * @param {Element} element
 * @returns {boolean}
 */
export function isFormControl(element) {
  return element.closest('form') !== null || element.matches('input, textarea, select');
}

/**
 * Whether the focus is on a form control inside `region`.
 * @param {Element} region
 * @returns {boolean}
 */
export function focusInForm(region) {
  const active = region.ownerDocument.activeElement;
  return active !== null && region.contains(active) && isFormControl(active);
}

/**
 * Whether a control holds a value other than the one it was rendered with.
 * @param {Element} control
 * @returns {boolean}
 */
function controlEdited(control) {
  if (control instanceof HTMLInputElement) {
    return control.type === 'checkbox' || control.type === 'radio'
      ? control.checked !== control.defaultChecked
      : control.value !== control.defaultValue;
  }
  if (control instanceof HTMLTextAreaElement) {
    return control.value !== control.defaultValue;
  }
  return control instanceof HTMLSelectElement && selectChanged({
    multiple: control.multiple,
    size: control.size,
    selectedIndex: control.selectedIndex,
    options: Array.from(control.options),
  });
}

/**
 * Whether a control inside `region` holds a value other than the one it was rendered with.
 * @param {Element} region
 * @returns {boolean}
 */
export function hasEdits(region) {
  return Array.from(region.querySelectorAll('input, textarea, select')).some(controlEdited);
}
