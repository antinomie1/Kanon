import { api } from '../api/client';
import type { ReplyPolicy } from '../types';
import { t } from './i18n.svelte';

/**
 * Localized rendering of a reply policy.
 *
 * Shared by the node editor and the instance form so the same policy is never described two
 * different ways. The node's own `description` string is preferred where it is available.
 */
export function describeReplyPolicy(policy: ReplyPolicy | null): string {
  if (!policy) return '-';
  switch (policy.mode) {
    case 'always':
      return t('reply.describe_always');
    case 'mention':
      return t('reply.describe_mention');
    case 'never':
      return t('reply.describe_never');
    case 'probability':
      return t('reply.describe_probability', {
        percent: Math.round(policy.probability * 100),
      });
  }
}

/**
 * Console state for the node-wide reply policy.
 *
 * The policy decides whether group and channel messages are answered at all; an instance may
 * override it, but this value is what every instance without an override inherits.
 */
class ReplyPolicyStore {
  policy = $state<ReplyPolicy | null>(null);
  /** Human-readable rendering produced by the node, shown instead of a hand-built label. */
  description = $state('');
  loading = $state(false);
  saving = $state(false);
  error = $state<string | null>(null);
  notice = $state<string | null>(null);

  async load() {
    this.loading = true;
    this.error = null;
    try {
      const res = await api.getReplyPolicy();
      this.policy = res.policy;
      this.description = res.description;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    } finally {
      this.loading = false;
    }
  }

  /** Applies a policy to the running node; the node echoes the effective value back. */
  async save(policy: ReplyPolicy): Promise<boolean> {
    this.saving = true;
    this.error = null;
    this.notice = null;
    try {
      const res = await api.setReplyPolicy(policy);
      this.policy = res.policy;
      this.description = res.description;
      this.notice = res.description;
      return true;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      this.saving = false;
    }
  }
}

export const replyPolicyStore = new ReplyPolicyStore();
