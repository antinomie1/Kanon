/** One short confirmation shown in the corner, optionally with a single action such as "Undo". */
export interface Toast {
  id: number;
  message: string;
  tone: 'ok' | 'bad' | 'info';
  action?: { label: string; run: () => void };
}

const DEFAULT_MS = 5000;
// Errors stay longer: the reader has to take in what failed, not just notice that something did.
const ERROR_MS = 9000;

/** Queue of transient messages. Each toast closes itself; acting on it closes it at once. */
class ToastStore {
  items = $state<Toast[]>([]);
  private nextId = 1;

  show(
    message: string,
    options: {
      tone?: Toast['tone'];
      action?: Toast['action'];
      ms?: number;
    } = {},
  ): number {
    const id = this.nextId++;
    const tone = options.tone ?? 'ok';
    // Keep the stack short; the oldest message is the least relevant one.
    this.items = [
      ...this.items.slice(-2),
      { id, message, tone, action: options.action },
    ];
    const ms = options.ms ?? (tone === 'bad' ? ERROR_MS : DEFAULT_MS);
    setTimeout(() => this.dismiss(id), ms);
    return id;
  }

  ok(message: string, action?: Toast['action']) {
    return this.show(message, { tone: 'ok', action });
  }

  error(message: string) {
    return this.show(message, { tone: 'bad' });
  }

  dismiss(id: number) {
    this.items = this.items.filter((toast) => toast.id !== id);
  }
}

export const toasts = new ToastStore();
