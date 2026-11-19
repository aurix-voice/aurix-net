/** Reports an error a listener of the game threw the way an `EventTarget` does: to the global
 *  error handler, off the stack of the client. */
export function report(error: unknown) {
  const global = globalThis as { reportError?: (error: unknown) => void };
  if (global.reportError) {
    global.reportError(error);
  } else {
    queueMicrotask(() => {
      throw error;
    });
  }
}
