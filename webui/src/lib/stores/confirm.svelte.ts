/** A question that needs an explicit yes before something irreversible happens. */
export interface ConfirmRequest {
  title: string;
  message?: string;
  /** Label of the confirming button; say what happens, e.g. "Delete instance". */
  confirm: string;
  cancel?: string;
  danger?: boolean;
}

interface Pending extends ConfirmRequest {
  resolve: (ok: boolean) => void;
}

/**
 * The single confirmation dialog of the console, replacing `window.confirm`, which cannot be
 * styled, blocks the page and shows the site origin instead of a useful title.
 */
class ConfirmStore {
  pending = $state<Pending | null>(null);

  ask(request: ConfirmRequest): Promise<boolean> {
    // A second question while one is open answers the first with "no" rather than stacking dialogs.
    this.pending?.resolve(false);
    return new Promise((resolve) => {
      this.pending = { ...request, resolve };
    });
  }

  answer(ok: boolean) {
    const pending = this.pending;
    this.pending = null;
    pending?.resolve(ok);
  }
}

export const confirmStore = new ConfirmStore();

/** Asks for confirmation and resolves to `true` only when the person confirms. */
export const confirmDialog = (request: ConfirmRequest) =>
  confirmStore.ask(request);
