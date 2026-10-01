<script lang="ts">
import { eventPolicyStore } from '../../stores/eventPolicy.svelte';
import { t } from '../../stores/i18n.svelte';
import { toasts } from '../../stores/toast.svelte';
import type { Capability, EventPolicy } from '../../types';
import Section from '../ui/Section.svelte';
import SupportBadge from '../ui/SupportBadge.svelte';
import Switch from '../ui/Switch.svelte';

$effect(() => {
  void eventPolicyStore.load();
});

/** The event switches, in the order they are shown, with the capability each one needs. */
const switches: {
  key: keyof EventPolicy;
  labelKey: string;
  hintKey: string;
  capabilities: Capability[];
}[] = [
  {
    key: 'welcome_members',
    labelKey: 'events.welcome',
    hintKey: 'events.welcome_hint',
    capabilities: ['member_join'],
  },
  {
    key: 'greet_on_join',
    labelKey: 'events.greet',
    hintKey: 'events.greet_hint',
    capabilities: ['bot_join', 'friend_add'],
  },
  {
    key: 'reply_to_poke',
    labelKey: 'events.poke',
    hintKey: 'events.poke_hint',
    capabilities: ['poke'],
  },
  {
    key: 'note_recalls',
    labelKey: 'events.recall',
    hintKey: 'events.recall_hint',
    capabilities: ['recall'],
  },
  {
    key: 'accept_friend_requests',
    labelKey: 'events.accept_friends',
    hintKey: 'events.accept_friends_hint',
    capabilities: ['friend_requests'],
  },
  {
    key: 'accept_group_invites',
    labelKey: 'events.accept_invites',
    hintKey: 'events.accept_invites_hint',
    capabilities: ['group_invites'],
  },
];

async function flip(key: keyof EventPolicy, value: boolean) {
  const current = eventPolicyStore.policy;
  if (!current) return;
  if (await eventPolicyStore.save({ ...current, [key]: value })) {
    toasts.ok(t('settings.saved_toast'));
  } else {
    toasts.error(eventPolicyStore.error ?? t('common.error'));
  }
}
</script>

<Section title={t('events.title')} hint={t('events.hint')}>
  {#if !eventPolicyStore.policy}
    <p class="m-0 hint">{eventPolicyStore.error ?? t('common.loading')}</p>
  {:else}
    {@const policy = eventPolicyStore.policy}
    {#each switches as item (item.key)}
      <div class="flex items-start gap-3 text-[14.5px]">
        <span class="min-w-0 flex-1">
          <span class="block font-semibold">{t(item.labelKey)}</span>
          <span class="block hint">{t(item.hintKey)}</span>
          <SupportBadge capabilities={item.capabilities} />
        </span>
        <Switch
          checked={policy[item.key]}
          disabled={eventPolicyStore.saving}
          label={t(item.labelKey)}
          onchange={(next) => void flip(item.key, next)}
        />
      </div>
    {/each}
  {/if}
</Section>
