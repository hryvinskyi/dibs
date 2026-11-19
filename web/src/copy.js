// @ts-check
/**
 * Copy buttons: `button[data-copy]` copies the text it carries, `button[data-copy-link]` the
 * address of the page shown. They are rendered hidden and shown only where the page can write to
 * the clipboard; the toast region says the copy was made, or that it failed, in its own words.
 */

/** How long the toast shows, in milliseconds. */
const TOAST_MS = 2600;

/** The buttons this module serves. */
const BUTTONS = 'button[data-copy], button[data-copy-link]';

/**
 * What the copy buttons are given.
 * @typedef {object} CopyDeps
 * @property {Pick<Clipboard, 'writeText'> | null | undefined} clipboard the page's
 *   `navigator.clipboard`, absent where the page may not write to it
 * @property {Pick<Location, 'href'>} location
 * @property {Element | null} toast the `[data-toast]` region, holding the words in `data-copied`
 *   and `data-copy-failed`
 */

/**
 * @typedef {object} Copy
 * @property {() => void} teardown
 * @property {(region: Element) => void} reveal shows the copy buttons of a region swapped in
 */

/**
 * The text a button copies: the page's address for `data-copy-link`, else its `data-copy` value.
 * @param {{ hasAttribute(name: string): boolean, getAttribute(name: string): string | null }} button
 * @param {string} href the address of the page shown
 * @returns {string}
 */
export function copiedText(button, href) {
  return button.hasAttribute('data-copy-link') ? href : (button.getAttribute('data-copy') ?? '');
}

/**
 * Shows every copy button inside `scope`.
 * @param {Element} scope
 * @returns {void}
 */
function unhide(scope) {
  scope.querySelectorAll(BUTTONS).forEach((button) => {
    if (button instanceof HTMLElement) {
      button.hidden = false;
    }
  });
}

/** @type {WeakSet<Element>} */
const mounted = new WeakSet();

/**
 * Serves `button[data-copy]` and `button[data-copy-link]` inside `root`. Mounting the same element
 * again, or on a page that cannot write to the clipboard, answers buttons that stay hidden.
 * @param {Element} root
 * @param {CopyDeps} deps
 * @returns {Copy}
 */
export function mountCopy(root, { clipboard, location, toast }) {
  if (clipboard === null || clipboard === undefined || mounted.has(root)) {
    return { teardown: () => {}, reveal: () => {} };
  }
  mounted.add(root);
  const controller = new AbortController();
  /** @type {ReturnType<typeof setTimeout> | undefined} */
  let timer;

  /** @param {string} attribute the toast's attribute holding the words */
  const say = (attribute) => {
    if (toast === null || controller.signal.aborted) {
      return;
    }
    clearTimeout(timer);
    toast.textContent = toast.getAttribute(attribute) ?? '';
    timer = setTimeout(() => {
      timer = undefined;
      toast.textContent = '';
    }, TOAST_MS);
  };

  unhide(root);

  root.addEventListener('click', (event) => {
    const button = event.target instanceof Element ? event.target.closest(BUTTONS) : null;
    if (button === null || !root.contains(button)) {
      return;
    }
    clipboard.writeText(copiedText(button, location.href)).then(
      () => say('data-copied'),
      (error) => {
        console.error('dibs: the text could not be copied', error);
        say('data-copy-failed');
      },
    );
  }, { signal: controller.signal });

  return {
    teardown: () => {
      controller.abort();
      mounted.delete(root);
      clearTimeout(timer);
      timer = undefined;
    },
    reveal: (region) => {
      if (!controller.signal.aborted) {
        unhide(region);
      }
    },
  };
}
