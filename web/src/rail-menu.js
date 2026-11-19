// @ts-check
/**
 * The rail as a menu on narrow screens: the rail is marked collapsible, its toggle button is
 * shown, and each click on the toggle opens or closes it. The state lives on the rail
 * (`data-open`) and on the toggle (`aria-expanded`); the stylesheet decides at which widths a
 * collapsible rail hides its sections.
 */

/** The toggle button inside the rail. */
const TOGGLE = 'button[data-rail-toggle]';

/**
 * @typedef {object} RailMenu
 * @property {() => void} teardown
 * @property {(rail: Element) => void} sync applies the state to a rail swapped in; any other
 *   region is left alone
 */

/**
 * The next `aria-expanded` value of the toggle; a toggle that does not say counts as closed.
 * @param {string | null} expanded
 * @returns {'true' | 'false'}
 */
export function toggled(expanded) {
  return expanded === 'true' ? 'false' : 'true';
}

/**
 * The rail inside `app`.
 * @param {Element} app
 * @returns {Element | null}
 */
function railOf(app) {
  return app.querySelector('#rail');
}

/**
 * Marks a rail collapsible, shows its toggle and sets the toggle from the rail's open state.
 * @param {Element} rail
 * @returns {void}
 */
function apply(rail) {
  rail.setAttribute('data-collapsible', 'true');
  const toggle = rail.querySelector(TOGGLE);
  if (toggle instanceof HTMLElement) {
    toggle.hidden = false;
    toggle.setAttribute('aria-expanded', rail.getAttribute('data-open') === 'true' ? 'true' : 'false');
  }
}

/** @type {WeakSet<Element>} */
const mounted = new WeakSet();

/**
 * Makes the rail collapsible on narrow screens. Mounting the same element again answers a menu
 * that does nothing.
 * @param {Element} app the `.app` element
 * @returns {RailMenu}
 */
export function mountRailMenu(app) {
  const rail = railOf(app);
  if (rail === null || mounted.has(app)) {
    return { teardown: () => {}, sync: () => {} };
  }
  mounted.add(app);
  const controller = new AbortController();
  apply(rail);

  app.addEventListener('click', (event) => {
    const toggle = event.target instanceof Element ? event.target.closest(TOGGLE) : null;
    const current = railOf(app);
    if (toggle === null || current === null || !current.contains(toggle)) {
      return;
    }
    const next = toggled(toggle.getAttribute('aria-expanded'));
    toggle.setAttribute('aria-expanded', next);
    if (next === 'true') {
      current.setAttribute('data-open', 'true');
    } else {
      current.removeAttribute('data-open');
    }
  }, { signal: controller.signal });

  return {
    teardown: () => {
      controller.abort();
      mounted.delete(app);
      const current = railOf(app);
      if (current === null) {
        return;
      }
      current.removeAttribute('data-collapsible');
      current.removeAttribute('data-open');
      const toggle = current.querySelector(TOGGLE);
      if (toggle instanceof HTMLElement) {
        toggle.hidden = true;
        toggle.setAttribute('aria-expanded', 'false');
      }
    },
    sync: (swapped) => {
      if (!controller.signal.aborted && swapped.id === 'rail') {
        apply(swapped);
      }
    },
  };
}
