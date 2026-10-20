// @ts-check
/**
 * Following the project's event stream from a page: the `EventSource` over the page's session
 * cookie, closed while the tab is hidden and reopened after the last event seen when it shows
 * again, and a check of the session when the server refuses the stream, which tells an ended
 * session from a stream that may be opened again later and from a cursor the log no longer reaches
 * back to: then the page reads its state again and the stream follows on from the newest event.
 */

import { ContractError, createSeen, parseEvent } from './events.js';
import { reconnectDelay } from './reconnect.js';
import { sessionVerdict } from './session-check.js';

/** How long the session check may take, in milliseconds. */
const SESSION_CHECK_MS = 5000;

/** How many event ids are remembered against duplicates. */
const SEEN_LIMIT = 512;

/**
 * `live` while events arrive, `reconnecting` while the connection is being restored, `ended` once
 * the page's session has ended; nothing is followed after `ended`.
 * @typedef {'live' | 'reconnecting' | 'ended'} StreamState
 */

/**
 * @typedef {object} FollowOptions
 * @property {import('./view-config.js').StreamConfig} config
 * @property {AbortSignal} signal ends the stream and every listener
 * @property {(event: import('./events.js').StreamEvent) => void} onEvent called once per event id
 * @property {(state: StreamState) => void} onState
 * @property {() => void} onResync called when what the page shows may have missed events the log no
 *   longer holds: the page reads its state again
 */

/**
 * Follows the stream until `signal` aborts or the session ends.
 * @param {FollowOptions} options
 * @returns {void}
 */
export function follow({ config, signal, onEvent, onState, onResync }) {
  const seen = createSeen(SEEN_LIMIT);
  let after = config.after;
  let attempt = 0;
  let ended = false;
  /** @type {EventSource | null} */
  let source = null;
  /** @type {ReturnType<typeof setTimeout> | undefined} */
  let retry;

  const close = () => {
    clearTimeout(retry);
    retry = undefined;
    if (source !== null) {
      source.close();
      source = null;
    }
  };

  /** @param {MessageEvent} message */
  const receive = (message) => {
    let event;
    try {
      event = parseEvent(message.data);
    } catch (error) {
      if (error instanceof ContractError) {
        console.error('dibs: an event did not have the expected shape', error.path);
        return;
      }
      throw error;
    }
    after = Math.max(after, event.id);
    if (seen.add(event.id)) {
      onEvent(event);
    }
  };

  const open = () => {
    close();
    if (ended || signal.aborted) {
      return;
    }
    const url = new URL(config.streamUrl, window.location.href);
    url.searchParams.set('after', String(after));
    const opened = new EventSource(url);
    source = opened;
    opened.addEventListener('open', () => {
      attempt = 0;
      onState('live');
    }, { signal });
    opened.addEventListener('message', receive, { signal });
    opened.addEventListener('error', () => {
      if (opened.readyState === EventSource.CLOSED) {
        checkSession().catch((error) => {
          console.error('dibs: the session could not be checked', error);
        });
      } else {
        onState('reconnecting');
      }
    }, { signal });
  };

  const checkSession = async () => {
    close();
    let status = 0;
    /** @type {unknown} */
    let body = null;
    try {
      const url = new URL(config.sessionUrl, window.location.href);
      url.searchParams.set('after', String(after));
      const response = await fetch(url, {
        credentials: 'same-origin',
        redirect: 'error',
        cache: 'no-store',
        signal: AbortSignal.any([signal, AbortSignal.timeout(SESSION_CHECK_MS)]),
      });
      status = response.status;
      if (status === 410) {
        body = await response.json();
      }
    } catch (error) {
      if (signal.aborted) {
        return;
      }
      console.error('dibs: the session check failed', error);
    }
    const verdict = sessionVerdict(status, body);
    if (verdict.kind === 'ended') {
      ended = true;
      onState('ended');
      return;
    }
    if (verdict.kind === 'resync') {
      after = verdict.from;
      attempt = 0;
      onResync();
      open();
      return;
    }
    onState('reconnecting');
    if (!document.hidden) {
      retry = setTimeout(open, reconnectDelay(attempt, Math.random));
      attempt += 1;
    }
  };

  document.addEventListener('visibilitychange', () => {
    if (document.hidden) {
      close();
    } else {
      open();
    }
  }, { signal });
  signal.addEventListener('abort', close, { once: true });

  if (!document.hidden) {
    open();
  }
}
