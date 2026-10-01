<script lang="ts">
import { t } from '../../stores/i18n.svelte';
import {
  describeReplyPolicy,
  replyPolicyStore,
} from '../../stores/replyPolicy.svelte';
import { toasts } from '../../stores/toast.svelte';
import type { ReplyMode, ReplyPolicy } from '../../types';
import Section from '../ui/Section.svelte';
import Seg from '../ui/Seg.svelte';
import SupportBadge from '../ui/SupportBadge.svelte';
import Switch from '../ui/Switch.svelte';

/** Draft of the node-wide reply policy, seeded from the node before it can be edited. */
let draft = $state<ReplyPolicy | null>(null);

// The draft is seeded from the node's answer rather than a guessed default; saving a guess would
// silently overwrite the real policy.
$effect(() => {
  void replyPolicyStore.load().then(() => {
    if (replyPolicyStore.policy) draft = { ...replyPolicyStore.policy };
  });
});

const dirty = $derived(
  draft !== null &&
    replyPolicyStore.policy !== null &&
    JSON.stringify(draft) !== JSON.stringify(replyPolicyStore.policy),
);

async function save() {
  if (!draft) return;
  if (await replyPolicyStore.save(draft)) {
    draft = { ...(replyPolicyStore.policy as ReplyPolicy) };
    toasts.ok(t('settings.saved_toast'));
  } else {
    toasts.error(replyPolicyStore.error ?? t('common.error'));
  }
}
</script>

{#if !draft}
  <p class="m-0 py-6 hint">{replyPolicyStore.error ?? t('common.loading')}</p>
{:else}
  <Section title={t('settings.reply_title')} hint={t('settings.reply_hint')}>
    <div>
      <Seg
        label={t('instances.reply_mode')}
        value={draft.mode}
        onchange={(next: ReplyMode) => draft && (draft.mode = next)}
        options={[
          { value: 'always', label: t('instances.reply_always') },
          { value: 'mention', label: t('instances.reply_mention') },
          { value: 'probability', label: t('instances.reply_random') },
          { value: 'never', label: t('instances.reply_never') },
        ]}
      />
      <p class="m-0 mt-2 hint">{describeReplyPolicy(draft)}</p>
    </div>
    {#if draft.mode === 'probability'}
      <label class="flex items-center gap-3 text-[14.5px]">
        <span class="whitespace-nowrap">{t('instances.reply_about')}</span>
        <input type="range" min="0" max="1" step="0.05" bind:value={draft.probability} class="max-w-[360px] flex-1" />
        <span class="w-11 font-extrabold tabular-nums">{Math.round(draft.probability * 100)}%</span>
      </label>
    {/if}
  </Section>

  <Section title={t('settings.reply_how')} hint={t('settings.reply_how_hint')}>
    <div class="flex items-start gap-3 text-[14.5px]">
      <span class="min-w-0 flex-1">
        <span class="block font-semibold">{t('reply.quote')}</span>
        <span class="block hint">{t('reply.quote_hint')}</span>
        <SupportBadge capabilities={['quote_reply']} />
      </span>
      <Switch checked={draft.quote_message} label={t('reply.quote')} onchange={(next) => draft && (draft.quote_message = next)} />
    </div>
    <div class="flex items-start gap-3 text-[14.5px]">
      <span class="min-w-0 flex-1">
        <span class="block font-semibold">{t('reply.acknowledge')}</span>
        <span class="block hint">{t('reply.acknowledge_hint')}</span>
        <SupportBadge capabilities={['acknowledge']} />
      </span>
      <Switch checked={draft.acknowledge} label={t('reply.acknowledge')} onchange={(next) => draft && (draft.acknowledge = next)} />
    </div>
  </Section>

  <div class="flex justify-end gap-2.5 border-t border-line py-5">
    <button
      type="button"
      class="btn"
      disabled={!dirty || replyPolicyStore.saving}
      onclick={() => replyPolicyStore.policy && (draft = { ...replyPolicyStore.policy })}
    >
      {t('instances.discard')}
    </button>
    <button type="button" class="btn btn-primary" disabled={!dirty || replyPolicyStore.saving} onclick={save}>
      {replyPolicyStore.saving ? t('instances.saving') : t('instances.save_changes')}
    </button>
  </div>
{/if}
