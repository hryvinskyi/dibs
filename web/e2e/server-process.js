// @ts-check
/**
 * The built `dibs-server` as a child process: a project and two members created with its admin
 * commands, then served on an ephemeral loopback port over a temporary database. The child's
 * environment holds only what the server reads.
 */

import { execFile, spawn } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { promisify } from 'node:util';

const run = promisify(execFile);

/** How long the server may take to report the address it listens on, in milliseconds. */
const LISTEN_TIMEOUT_MS = 30_000;

/** The project every case runs in. */
export const PROJECT = 'widgets';

/**
 * @typedef {object} RunningServer
 * @property {string} url the base URL, `http://127.0.0.1:<port>`
 * @property {string} adminToken the admin `alice`'s token
 * @property {string} memberToken the member `bob`'s token
 * @property {string} database the path of the server's database file
 * @property {() => Promise<void>} stop ends the server and removes its database
 */

/**
 * The token an admin command printed on standard output.
 * @param {string} output
 * @returns {string}
 */
function tokenOf(output) {
  const token = output.trim();
  if (!/^dibs_\S+$/.test(token)) {
    throw new Error('standard output of the admin command is not a token alone');
  }
  return token;
}

/**
 * Waits for the server's log to say where it listens.
 * @param {import('node:child_process').ChildProcessByStdio<null, import('node:stream').Readable, null>} child
 * @returns {Promise<string>} the address, `127.0.0.1:<port>`
 */
function listeningAddress(child) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error('the server did not report an address in time')),
      LISTEN_TIMEOUT_MS,
    );
    child.once('exit', (code) => {
      clearTimeout(timer);
      reject(new Error(`the server exited with status ${code} before it listened`));
    });
    // The log is read until the server ends, so an unread pipe never stalls it.
    createInterface({ input: child.stdout }).on('line', (line) => {
      let entry;
      try {
        entry = JSON.parse(line);
      } catch {
        return;
      }
      const fields = entry?.fields;
      if (fields?.message === 'listening' && typeof fields.address === 'string') {
        clearTimeout(timer);
        resolve(fields.address);
      }
    });
  });
}

/**
 * Starts a server with the project `widgets`, its admin `alice` and its member `bob`.
 * @param {string} binary the path of the built `dibs-server`
 * @returns {Promise<RunningServer>}
 */
export async function startServer(binary) {
  const scratch = await mkdtemp(join(tmpdir(), 'dibs-e2e-'));
  const env = { DIBS_DATABASE: join(scratch, 'dibs.db'), HOME: scratch };
  try {
    const init = await run(
      binary,
      ['admin', 'init', '--project', PROJECT, '--admin', 'alice', '--name', 'Alice Example'],
      { env },
    );
    const child = spawn(binary, ['serve'], {
      env: { ...env, DIBS_LISTEN: '127.0.0.1:0' },
      stdio: ['ignore', 'pipe', 'ignore'],
    });
    const stop = async () => {
      child.kill();
      await rm(scratch, { recursive: true, force: true });
    };
    try {
      const address = await listeningAddress(child);
      const member = await run(
        binary,
        ['admin', 'member', 'add', 'bob', '--project', PROJECT, '--name', 'Bob Example'],
        { env },
      );
      return {
        url: `http://${address}`,
        adminToken: tokenOf(init.stdout),
        memberToken: tokenOf(member.stdout),
        database: env.DIBS_DATABASE,
        stop,
      };
    } catch (error) {
      await stop();
      throw error;
    }
  } catch (error) {
    await rm(scratch, { recursive: true, force: true });
    throw error;
  }
}
