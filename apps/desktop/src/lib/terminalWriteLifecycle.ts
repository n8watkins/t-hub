interface WritableTerminal {
  write(data: string | Uint8Array, callback?: () => void): void;
  dispose(): void;
}

/**
 * Keeps xterm buffer mutations and disposal behind its asynchronous write parser.
 *
 * xterm accepts writes synchronously but parses them later.
 * Calling clear, reset, or dispose while accepted writes are still parsing can
 * leave the input buffer and renderer observing different buffer generations.
 */
export class TerminalWriteLifecycle {
  private pendingWrites = 0;
  private retired = false;
  private readonly idleActions: Array<() => void> = [];

  constructor(private readonly terminal: WritableTerminal) {}

  write(data: string | Uint8Array): boolean {
    if (this.retired) return false;

    this.pendingWrites += 1;
    let settled = false;
    const settle = (): void => {
      if (settled) return;
      settled = true;
      this.pendingWrites -= 1;
      this.flushIdleActions();
    };

    try {
      this.terminal.write(data, settle);
    } catch (error) {
      settle();
      throw error;
    }
    return true;
  }

  afterWrites(action: () => void): void {
    if (this.pendingWrites === 0) {
      action();
      return;
    }
    this.idleActions.push(action);
  }

  waitForWrites(): Promise<void> {
    if (this.pendingWrites === 0) return Promise.resolve();
    return new Promise((resolve) => this.afterWrites(resolve));
  }

  disposeWhenIdle(): void {
    if (this.retired) return;
    this.retired = true;
    this.afterWrites(() => this.terminal.dispose());
  }

  private flushIdleActions(): void {
    if (this.pendingWrites !== 0) return;
    const actions = this.idleActions.splice(0, this.idleActions.length);
    for (const action of actions) action();
  }
}
