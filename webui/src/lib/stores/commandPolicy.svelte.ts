import { api } from '../api/client';
import type { CommandPolicy } from '../types';

/**
 * Console state for the node-wide command permissions: who the bot's administrators are and which
 * commands only they may run.
 */
class CommandPolicyStore {
  policy = $state<CommandPolicy | null>(null);
  loading = $state(false);
  saving = $state(false);
  error = $state<string | null>(null);
  notice = $state<string | null>(null);

  async load() {
    this.loading = true;
    this.error = null;
    try {
      this.policy = (await api.getCommandPolicy()).policy;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    } finally {
      this.loading = false;
    }
  }

  /** Saves a policy; the node normalizes it and echoes what it enforces. */
  async save(policy: CommandPolicy): Promise<boolean> {
    this.saving = true;
    this.error = null;
    this.notice = null;
    try {
      this.policy = (await api.setCommandPolicy(policy)).policy;
      this.notice = 'saved';
      return true;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      this.saving = false;
    }
  }
}

export const commandPolicyStore = new CommandPolicyStore();
