// @ts-check
/**
 * A page of claims that offers a reload when they change: on an event of the kinds the page names,
 * its reload banner shows; the page itself is never rebuilt in the browser. When the session is
 * gone, the page's session-ended banner shows instead. Every wording comes from the template.
 */

import { isChange } from './events.js';
import { follow } from './live-stream.js';
import { parseStatusConfig } from './view-config.js';

/** @type {WeakSet<HTMLElement>} */
const mounted = new WeakSet();

/**
 * @param {HTMLElement} root
 * @param {string} selector
 * @returns {void}
 */
function reveal(root, selector) {
  root.querySelectorAll(selector).forEach((element) => {
    if (element instanceof HTMLElement) {
      element.hidden = false;
    }
  });
}

/**
 * Mounts the live behaviour on a page's root. Mounting the same element again does nothing.
 * @param {HTMLElement} root the element carrying `data-status-view`
 * @returns {() => void} the teardown
 */
export function mount(root) {
  if (mounted.has(root)) {
    return () => {};
  }
  let config;
  try {
    config = parseStatusConfig(root.dataset);
  } catch (error) {
    console.error('dibs: the page is missing its live configuration', error);
    return () => {};
  }
  mounted.add(root);
  const controller = new AbortController();
  const { kinds } = config;
  let ended = false;
  follow({
    config,
    signal: controller.signal,
    onEvent: (event) => {
      if (!ended && isChange(event, kinds)) {
        reveal(root, '[data-changed]');
      }
    },
    onState: (state) => {
      if (state === 'ended') {
        ended = true;
        root.querySelectorAll('[data-changed]').forEach((element) => {
          if (element instanceof HTMLElement) {
            element.hidden = true;
          }
        });
        reveal(root, '[data-session-ended]');
      }
    },
    onResync: () => {
      if (!ended) {
        reveal(root, '[data-changed]');
      }
    },
  });
  return () => {
    controller.abort();
    mounted.delete(root);
  };
}
