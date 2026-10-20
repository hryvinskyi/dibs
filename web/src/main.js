// @ts-check
/**
 * The entry point of every page: records the viewer's offset, localizes the page's times and
 * mounts the live views it carries.
 */

import { mount as mountBoard } from './board-view.js';
import { localizeTimes, rememberZone } from './local-times.js';
import { mount as mountStatus } from './status-view.js';
import { mount as mountThread } from './thread-view.js';

/**
 * @param {string} selector
 * @param {(root: HTMLElement) => unknown} mount
 * @returns {void}
 */
function mountAll(selector, mount) {
  document.querySelectorAll(selector).forEach((element) => {
    if (element instanceof HTMLElement) {
      mount(element);
    }
  });
}

function start() {
  rememberZone(document);
  localizeTimes(document);
  mountAll('[data-thread-view]', mountThread);
  mountAll('[data-status-view]', mountStatus);
  mountAll('[data-board-view]', mountBoard);
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', start, { once: true });
} else {
  start();
}
