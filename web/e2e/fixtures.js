// @ts-check
/**
 * The cases' fixtures, and the composition root of the browser suite: a server of its own for each
 * case, the path of its binary read from the environment here and nowhere else, and a page that
 * fails its test when the browser reports a content security policy violation.
 *
 * Each case starts from an empty project, so that areas, slots and claims one case takes never
 * stand in another's way, and what a page shows depends on that case alone.
 */

import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test as base, expect } from '@playwright/test';

import { watchPolicy } from './csp-watch.js';
import { startServer } from './server-process.js';

/** The repository root, where cargo's `target/` directory is. */
const ROOT = fileURLToPath(new URL('../..', import.meta.url));

/**
 * The built server: `DIBS_SERVER_BIN` when set, else the debug build in the workspace.
 * @returns {string}
 */
function serverBinary() {
  const configured = process.env.DIBS_SERVER_BIN;
  const binary =
    configured && configured !== ''
      ? configured
      : join(ROOT, 'target', 'debug', process.platform === 'win32' ? 'dibs-server.exe' : 'dibs-server');
  if (!existsSync(binary)) {
    throw new Error(`${binary} is missing; build it with: cargo build -p dibs-server`);
  }
  return binary;
}

/**
 * @typedef {import('@playwright/test').PlaywrightTestArgs & import('@playwright/test').PlaywrightTestOptions} TestArgs
 * @typedef {import('@playwright/test').PlaywrightWorkerArgs & import('@playwright/test').PlaywrightWorkerOptions} WorkerArgs
 * @typedef {{ dibs: import('./server-process.js').RunningServer }} ServerFixture
 */

const fixtures = {
  /**
   * A server of the case's own, stopped when the case ends.
   * @param {object} _fixtures
   * @param {(server: import('./server-process.js').RunningServer) => Promise<void>} use
   */
  dibs: async ({}, use) => {
    const server = await startServer(serverBinary());
    try {
      await use(server);
    } finally {
      await server.stop();
    }
  },

  /**
   * @param {{ page: import('@playwright/test').Page }} fixtures
   * @param {(page: import('@playwright/test').Page) => Promise<void>} use
   */
  page: async ({ page }, use) => {
    const policy = await watchPolicy(page);
    await use(page);
    expect(policy.violations(), 'the page reported a content security policy violation').toEqual([]);
  },
};

/**
 * `base` extended with the fixtures above; `extend` cannot infer the fixtures' types in
 * JavaScript, so the result is typed here.
 * @type {import('@playwright/test').TestType<TestArgs & ServerFixture, WorkerArgs>}
 */
export const test = base.extend(fixtures);

export { expect };
