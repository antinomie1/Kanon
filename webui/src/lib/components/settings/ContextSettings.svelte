<script lang="ts">
import { contextPolicyStore } from '../../stores/contextPolicy.svelte';
import { t } from '../../stores/i18n.svelte';
import { toasts } from '../../stores/toast.svelte';
import type { Capability, ContextPolicy } from '../../types';
import Section from '../ui/Section.svelte';
import SupportBadge from '../ui/SupportBadge.svelte';
import Switch from '../ui/Switch.svelte';

$effect(() => {
  void contextPolicyStore.load();
});

const switches: {
  key: keyof ContextPolicy;
  labelKey: string;
  hintKey: string;
  capabilities?: Capability[];
}[] = [
  {
    key: 'include_channel_id',
    labelKey: 'context.channel_id',
    hintKey: 'context.channel_id_hint',
  },
  {
    key: 'include_sender_id',
    labelKey: 'context.sender_id',
    hintKey: 'context.sender_id_hint',
  },
  {
    key: 'include_timestamp',
    labelKey: 'context.timestamp',
    hintKey: 'context.timestamp_hint',
  },
  {
    key: 'expand_forward',
    labelKey: 'context.expand_forward',
    hintKey: 'context.expand_forward_hint',
    capabilities: ['forward_content'],
  },
];

/** Each switch applies at once: they are independent, reversible and affect only the next turn. */
async function flip(key: keyof ContextPolicy, value: boolean) {
  const current = contextPolicyStore.policy;
  if (!current) return;
  if (await contextPolicyStore.save({ ...current, [key]: value })) {
    toasts.ok(t('settings.saved_toast'));
  } else {
    toasts.error(contextPolicyStore.error ?? t('common.error'));
  }
}
</script>

<Section title={t('context.title')} hint={t('context.hint')}>
  {#if !contextPolicyStore.policy}
    <p class="m-0 hint">{contextPolicyStore.error ?? t('common.loading')}</p>
  {:else}
    {@const policy = contextPolicyStore.policy}
    {#each switches as item (item.key)}
      <div class="flex items-start gap-3 text-[14.5px]">
        <span class="min-w-0 flex-1">
          <span class="block font-medium">{t(item.labelKey)}</span>
          <span class="block hint">{t(item.hintKey)}</span>
          {#if item.capabilities}<SupportBadge capabilities={item.capabilities} />{/if}
        </span>
        <Switch
          checked={policy[item.key]}
          disabled={contextPolicyStore.saving}
          label={t(item.labelKey)}
          onchange={(next) => void flip(item.key, next)}
        />
      </div>
    {/each}
  {/if}
</Section>
