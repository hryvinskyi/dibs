// @ts-check
/**
 * Which messages appended live take the form the server renders them in: an answer replaces those
 * it was asked for that it holds, and settles every one it was asked for. Pure, so a test reaches
 * it without a page.
 */

/**
 * What an answer settles for the messages appended live: those of `asked` that `rendered` holds are
 * replaced; every one the answer was asked for is settled, so only those appended since it was
 * asked for still wait.
 * @param {readonly number[]} waiting the messages waiting for their rendered form now
 * @param {readonly number[]} asked the messages the answer was asked for
 * @param {ReadonlySet<number>} rendered the messages the answer holds
 * @returns {{ replace: number[], waiting: number[] }}
 */
export function settle(waiting, asked, rendered) {
  const settled = new Set(asked);
  return {
    replace: asked.filter((id) => rendered.has(id)),
    waiting: waiting.filter((id) => !settled.has(id)),
  };
}
