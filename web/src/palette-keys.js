// @ts-check
/**
 * What a key does to the search palette: open it, close it, move the selection through its hits,
 * or open the selected hit here or outside. A pure reducer over the palette's state, and the test
 * of whether the platform's command key is the Mac's.
 */

/**
 * The palette's state when a key is pressed.
 * @typedef {object} PaletteState
 * @property {boolean} open whether the palette is shown
 * @property {boolean} typing whether the key was typed into a field
 * @property {number} count how many hits it lists
 * @property {number} index the hit selected
 */

/**
 * The modifier keys held with a key.
 * @typedef {{ meta: boolean, ctrl: boolean }} Modifiers
 */

/** @typedef {'none' | 'open' | 'close' | 'move' | 'here' | 'out'} PaletteAction */

/** @typedef {{ action: PaletteAction, index: number }} PaletteStep */

/**
 * What a key does while the palette is open, by the key's name.
 * @type {Readonly<Record<string, (state: PaletteState, command: boolean) => PaletteStep>>}
 */
const WHILE_OPEN = Object.freeze({
  Escape: (state) => ({ action: 'close', index: state.index }),
  ArrowDown: (state) => moved(state, state.index + 1),
  ArrowUp: (state) => moved(state, state.index - 1),
  Enter: (state, command) =>
    state.count > 0 ? { action: command ? 'out' : 'here', index: state.index } : stay(state),
});

/**
 * Nothing happens.
 * @param {PaletteState} state
 * @returns {PaletteStep}
 */
function stay(state) {
  return { action: 'none', index: state.index };
}

/**
 * The selection moved to `index`, kept between the first hit and the last; nothing happens
 * without a hit.
 * @param {PaletteState} state
 * @param {number} index
 * @returns {PaletteStep}
 */
function moved(state, index) {
  if (state.count <= 0) {
    return stay(state);
  }
  return { action: 'move', index: Math.min(state.count - 1, Math.max(0, index)) };
}

/**
 * Whether the key is the palette's own shortcut: `K` with the command key or Control.
 * @param {string} key
 * @param {Modifiers} mods
 * @returns {boolean}
 */
function isShortcut(key, mods) {
  return (mods.meta || mods.ctrl) && key.toLowerCase() === 'k';
}

/**
 * What a key does to the palette.
 * @param {PaletteState} state
 * @param {string} key the `KeyboardEvent`'s key
 * @param {Modifiers} mods
 * @returns {PaletteStep}
 */
export function next(state, key, mods) {
  if (isShortcut(key, mods)) {
    return { action: state.open ? 'close' : 'open', index: 0 };
  }
  if (!state.open) {
    const opens = key === '/' && !state.typing && !mods.meta && !mods.ctrl;
    return opens ? { action: 'open', index: 0 } : stay(state);
  }
  const handler = Object.hasOwn(WHILE_OPEN, key) ? WHILE_OPEN[key] : undefined;
  return handler === undefined ? stay(state) : handler(state, mods.meta || mods.ctrl);
}

/**
 * Whether a platform's name is the Mac's, where the command key is `⌘`.
 * @param {string} platform
 * @returns {boolean}
 */
export function isMac(platform) {
  return /mac/i.test(platform);
}
