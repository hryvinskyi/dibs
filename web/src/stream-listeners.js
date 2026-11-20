// @ts-check
/**
 * The listeners a page's one event stream is handed to: each is called for every event, state and
 * resync until it unsubscribes, and one that throws does not keep the others from theirs.
 */

/**
 * The callbacks of `live-stream.js`'s `follow`, each optional.
 * @typedef {object} StreamListener
 * @property {(event: import('./events.js').StreamEvent) => void} [onEvent]
 * @property {(state: import('./live-stream.js').StreamState) => void} [onState]
 * @property {(from?: number) => void} [onResync] given the cursor the stream resumed from, when known
 */

/**
 * @typedef {object} StreamListeners
 * @property {(listener: StreamListener) => () => void} subscribe adds `listener` until the returned
 *   function is called
 * @property {(call: (listener: StreamListener) => void) => void} each calls `call` with every
 *   listener subscribed now
 * @property {() => void} clear unsubscribes every listener
 */

/**
 * An empty set of listeners; a listener that throws is reported to `onError`.
 * @param {(error: unknown) => void} onError
 * @returns {StreamListeners}
 */
export function streamListeners(onError) {
  /** @type {Set<StreamListener>} */
  const listeners = new Set();
  return {
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    each(call) {
      for (const listener of [...listeners]) {
        try {
          call(listener);
        } catch (error) {
          onError(error);
        }
      }
    },
    clear() {
      listeners.clear();
    },
  };
}
