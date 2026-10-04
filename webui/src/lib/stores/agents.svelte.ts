import { api } from '../api/client';
import type { AgentsResponse } from '../types';
import { t } from './i18n.svelte';

/** Display name of an agent; an agent the console has no translation for shows its identifier. */
export function agentName(id: string): string {
  return id === 'builtin' ? t('agents.builtin') : id === 'dsh' ? 'deepseek-harness' : id;
}

/**
 * Console state for agent selection: which agents exist and which one the node uses by default.
 *
 * Availability comes from the running node, including its optional compiled backends.
 * Instances override the default with their own `agent` field.
 */
class AgentsStore {
  catalog = $state<AgentsResponse | null>(null);
  saving = $state(false);
  error = $state<string | null>(null);

  get agents(): string[] {
    return this.catalog?.agents ?? [];
  }

  get defaultAgent(): string | null {
    return this.catalog?.default_agent ?? null;
  }

  async load() {
    this.error = null;
    try {
      this.catalog = await api.getAgents();
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    }
  }

  /** Applies a new node default; the node validates, stores and echoes it back. */
  async setDefault(agent: string): Promise<boolean> {
    this.saving = true;
    this.error = null;
    try {
      this.catalog = await api.setDefaultAgent(agent);
      return true;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      this.saving = false;
    }
  }
}

export const agentsStore = new AgentsStore();
