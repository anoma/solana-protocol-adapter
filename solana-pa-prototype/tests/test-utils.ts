/**
 * Shared test utilities for solana-pa-prototype tests.
 *
 * Provides unified error handling patterns for Anchor-based tests,
 * reducing code duplication and ensuring consistent error assertions.
 */

/**
 * Extracts error information from an Anchor/Solana error.
 *
 * Handles multiple error formats:
 * - Anchor errors: e.error.errorMessage, e.error.logs
 * - Solana errors: e.logs
 * - Generic errors: e.toString()
 *
 * @param e - The caught error object
 * @returns Object with message and logs array for pattern matching
 */
export function extractErrorInfo(e: unknown): { message: string; logs: string[] } {
  const err = e as {
    error?: { errorMessage?: string; logs?: string[] };
    logs?: string[];
    toString?: () => string;
  };

  const message = err?.error?.errorMessage ?? err?.toString?.() ?? "";
  const logs: string[] = err?.logs ?? err?.error?.logs ?? [];

  return { message, logs };
}

/**
 * Combines error message and logs into a single searchable string.
 *
 * @param e - The caught error object
 * @returns A newline-joined string of message + all logs
 */
export function getErrorHaystack(e: unknown): string {
  const { message, logs } = extractErrorInfo(e);
  return [message, ...logs].join("\n");
}

/**
 * Asserts that a promise rejects with an error matching the given pattern.
 *
 * This is the recommended way to test for expected errors in Anchor tests.
 * It handles the various error formats from Anchor, Solana, and generic JS errors.
 *
 * @param promise - The promise expected to reject
 * @param pattern - A RegExp or string to match against the error
 * @param failMessage - Optional message if the promise doesn't reject
 * @throws AssertionError if promise resolves or error doesn't match pattern
 *
 * @example
 * ```typescript
 * await assertRejectsWithError(
 *   program.methods.emergencyStop().rpc(),
 *   /Unauthorized|ConstraintHasOne/i,
 *   "Should fail for non-authority"
 * );
 * ```
 */
export async function assertRejectsWithError(
  promise: Promise<unknown>,
  pattern: RegExp | string,
  failMessage?: string
): Promise<void> {
  const { assert } = await import("chai");

  try {
    await promise;
    assert.fail(failMessage ?? `Expected promise to reject with pattern: ${pattern}`);
  } catch (e: unknown) {
    // Don't catch assertion errors from assert.fail above
    if (e instanceof Error && e.name === "AssertionError") {
      throw e;
    }

    const haystack = getErrorHaystack(e);
    const regex = typeof pattern === "string" ? new RegExp(pattern, "i") : pattern;

    assert.match(
      haystack,
      regex,
      `Error did not match expected pattern.\nPattern: ${pattern}\nActual error: ${haystack.slice(0, 500)}`
    );
  }
}

/**
 * Constants for TxData expiration tests.
 * These must match the values in programs/solana-pa-prototype/src/state.rs
 */
export const TX_DATA_EXPIRY = {
  MIN_SLOTS: 100,
  MAX_SLOTS: 216_000,
} as const;

/**
 * Chunk size for TxData writes.
 * Chosen to fit comfortably within Solana transaction size limits.
 */
export const TX_DATA_CHUNK_SIZE = 700;
