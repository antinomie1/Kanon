import { api } from '../api/client';
import type { ContextPolicy } from '../types';
import { t } from './i18n.svelte';

/** Localized rendering of a context policy, shared by the node editor and the instance form. */
export function describeContextPolicy(policy: ContextPolicy | null): string {
  if (!policy) return '-';
  const parts: string[] = [];
  if (policy.include_channel_id) parts.push(t('context.channel_id'));
  if (policy.include_sender_id) parts.push(t('context.sender_id'));
  if (policy.include_timestamp) parts.push(t('context.timestamp'));
  return parts.length > 0 ? parts.join(' + ') : t('context.none');
}

/**
 * Console state for the node-wide context-extras policy.
 *
 * The policy decides whether the sender id and the message time are prepended to the model prompt.
 * An instance may override it; this value is what every instance without an override inherits.
 */
class ContextPolicyStore {
  policy = $state<ContextPolicy | null>(null);
  loading = $state(false);
  saving = $state(false);
  error = $state<string | null>(null);
  notice = $state<string | null>(null);

  async load() {
    this.loading = true;
    this.error = null;
    try {
      const res = await api.getContextPolicy();
      this.policy = res.policy;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    } finally {
      this.loading = false;
    }
  }

  /** Applies a policy to the running node; the node echoes the effective value back. */
  async save(policy: ContextPolicy): Promise<boolean> {
    this.saving = true;
    this.error = null;
    this.notice = null;
    try {
      const res = await api.setContextPolicy(policy);
      this.policy = res.policy;
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

export const contextPolicyStore = new ContextPolicyStore();
