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
  private idleFlushScheduled = false;
  private readonly idleActions: Array<{ key?: string; action: () => void }> = [];

  constructor(private readonly terminal: WritableTerminal) {}

  write(data: string | Uint8Array): boolean {
    if (this.retired) return false;

    this.pendingWrites += 1;
    let settled = false;
    const settle = (): void => {
      if (settled) return;
      settled = true;
      this.pendingWrites -= 1;
      this.scheduleIdleFlush();
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
    if (this.pendingWrites === 0 && !this.idleFlushScheduled) {
      action();
      return;
    }
    this.idleActions.push({ action });
  }

  afterWritesCoalesced(key: string, action: () => void): void {
    if (this.pendingWrites === 0 && !this.idleFlushScheduled) {
      action();
      return;
    }
    const pending = this.idleActions.find((entry) => entry.key === key);
    if (pending) {
      pending.action = action;
      return;
    }
    this.idleActions.push({ key, action });
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

  private scheduleIdleFlush(): void {
    if (this.pendingWrites !== 0 || this.idleFlushScheduled) return;
    this.idleFlushScheduled = true;
    // xterm invokes a write callback before it advances its own buffer offset,
    // clears the write queue, and emits onWriteParsed. Running a resize or clear
    // directly from that callback re-enters the parser with half-settled buffer
    // bookkeeping. Leave the xterm stack first, then mutate its buffer.
    queueMicrotask(() => {
      this.idleFlushScheduled = false;
      if (this.pendingWrites !== 0) return;
      this.flushIdleActions();
    });
  }

  private flushIdleActions(): void {
    if (this.pendingWrites !== 0) return;
    const actions = this.idleActions.splice(0, this.idleActions.length);
    for (const { action } of actions) action();
  }
}
