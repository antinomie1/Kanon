import { api } from '../api/client';
import type { EventPolicy } from '../types';

/**
 * Console state for the node-wide event policy: which platform notices (a member joining, a poke,
 * the bot being added somewhere, a recall) the bot reacts to.
 */
class EventPolicyStore {
  policy = $state<EventPolicy | null>(null);
  loading = $state(false);
  saving = $state(false);
  error = $state<string | null>(null);
  notice = $state<string | null>(null);

  async load() {
    this.loading = true;
    this.error = null;
    try {
      this.policy = (await api.getEventPolicy()).policy;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    } finally {
      this.loading = false;
    }
  }

  /** Applies a policy to the running node; the node echoes the effective value back. */
  async save(policy: EventPolicy): Promise<boolean> {
    this.saving = true;
    this.error = null;
    this.notice = null;
    try {
      this.policy = (await api.setEventPolicy(policy)).policy;
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

export const eventPolicyStore = new EventPolicyStore();
