<script lang="ts">
import { TriangleAlert } from 'lucide-svelte';
import { describeContextPolicy } from '../../stores/contextPolicy.svelte';
import { t } from '../../stores/i18n.svelte';
import { instancesStore, type PolicyKind } from '../../stores/instances.svelte';
import { router } from '../../stores/router.svelte';
import type { BashScope, ItemPolicy, SessionScope } from '../../types';
import CommandPolicyEditor from '../ui/CommandPolicyEditor.svelte';
import Section from '../ui/Section.svelte';
import Seg from '../ui/Seg.svelte';
import SupportBadge from '../ui/SupportBadge.svelte';
import Switch from '../ui/Switch.svelte';

/**
 * The settings most instances never change: group memory, what the model is told, command
 * permissions, Bash and per-instance extension overrides. Kept out of the first screen so the
 * common path stays short, and opened automatically when an instance already overrides any.
 */

const store = instancesStore;

const kinds: { kind: PolicyKind; titleKey: string }[] = [
  { kind: 'plugins', titleKey: 'instances.section_plugins' },
  { kind: 'skills', titleKey: 'instances.section_skills' },
  { kind: 'mcp', titleKey: 'instances.section_mcp' },
];

const bashScopes: { value: BashScope; labelKey: string; hintKey: string }[] = [
  {
    value: 'disabled',
    labelKey: 'instances.bash_disabled',
    hintKey: 'instances.bash_disabled_hint',
  },
  {
    value: 'own_context',
    labelKey: 'instances.bash_own',
    hintKey: 'instances.bash_own_hint',
  },
  {
    value: 'shared_context',
    labelKey: 'instances.bash_shared',
    hintKey: 'instances.bash_shared_hint',
  },
];

const contextSwitches = [
  { field: 'formIncludeChannelId', key: 'context.channel_id' },
  { field: 'formIncludeSenderId', key: 'context.sender_id' },
  { field: 'formIncludeTimestamp', key: 'context.timestamp' },
  { field: 'formExpandForward', key: 'context.expand_forward' },
] as const;
</script>

<div class="mt-4 divide-y divide-line border-t border-line">
  <Section collapsible title={t('group.title')} hint={t('group.hint')}>
    {#if store.formConversationMode === 'simulation'}
      <p class="m-0 hint">{t('instances.simulation_memory')}</p>
    {:else}
    <div>
      <Seg
        label={t('group.title')}
        value={store.formSessionScope}
        onchange={(next: SessionScope) => (store.formSessionScope = next)}
        options={[
          { value: 'user', label: t('instances.scope_user') },
          { value: 'group', label: t('instances.scope_group') },
        ]}
      />
      <p class="m-0 mt-2 hint">
        {store.formSessionScope === 'group' ? t('group.scope_group_hint') : t('group.scope_user_hint')}
      </p>
    </div>
    <div class="flex items-start gap-3 text-[14.5px]">
      <span class="min-w-0 flex-1">
        <span class="block font-medium">{t('group.observe')}</span>
        <span class="block hint">{t('group.observe_hint')}</span>
        <SupportBadge capabilities={['group_messages']} />
      </span>
      <Switch
        checked={store.formObserveGroup}
        label={t('group.observe')}
        onchange={(next) => (store.formObserveGroup = next)}
      />
    </div>
    {/if}
  </Section>

  <Section collapsible title={t('context.title')} hint={t('context.hint')}>
    <div class="flex flex-wrap items-center gap-x-3 gap-y-1 text-[14.5px]">
      <span class="flex-1 font-medium">{t('instances.use_global_setting')}</span>
      {#if store.nodeContextPolicy}
        <span class="text-[13px] text-fg2">
          {t('instances.global_is', { policy: describeContextPolicy(store.nodeContextPolicy) })}
        </span>
      {/if}
      <Switch
        checked={store.formContextInherit}
        label={t('instances.use_global_setting')}
        onchange={(next) => (store.formContextInherit = next)}
      />
    </div>
    {#if !store.formContextInherit}
      {#each contextSwitches as item (item.field)}
        <div class="flex items-center gap-3 text-[14.5px]">
          <span class="flex-1">{t(item.key)}</span>
          <Switch
            checked={store[item.field]}
            label={t(item.key)}
            onchange={(next) => (store[item.field] = next)}
          />
        </div>
      {/each}
    {/if}
  </Section>

  <Section collapsible title={t('commands.title')} hint={t('instances.commands_hint')}>
    <div class="flex flex-wrap items-center gap-x-3 gap-y-1 text-[14.5px]">
      <span class="flex-1 font-medium">{t('instances.use_global_setting')}</span>
      <Switch
        checked={store.formCommandInherit}
        label={t('instances.use_global_setting')}
        onchange={(next) => (store.formCommandInherit = next)}
      />
    </div>
    {#if store.formCommandInherit}
      <p class="m-0 hint">
        {t('instances.commands_inherit_hint', {
          admins: store.nodeCommandPolicy?.admins.join(', ') || t('instances.no_admins'),
        })}
      </p>
    {:else}
      <CommandPolicyEditor bind:draft={store.formCommandDraft} />
    {/if}
  </Section>

  <Section collapsible title={t('bash.title')} hint={t('instances.bash_hint')}>
    {#if store.formConversationMode === 'simulation'}
      <p class="m-0 hint">{t('instances.simulation_bash')}</p>
    {:else}
    <div class="flex flex-col gap-2" role="radiogroup" aria-label={t('bash.title')}>
      {#each bashScopes as scope (scope.value)}
        {@const on = store.formBash === scope.value}
        <label
          class="flex cursor-pointer items-start gap-3 rounded-xl px-3.5 py-3 {on
            ? 'bg-accent-tint shadow-[inset_0_0_0_2px_var(--k-accent)]'
            : 'bg-sunk'}"
        >
          <input
            type="radio"
            name="bash-scope"
            class="check mt-0.5"
            value={scope.value}
            bind:group={store.formBash}
          />
          <span class="min-w-0">
            <span class="block text-[14.5px] font-medium {on ? 'text-accent-fg' : ''}">{t(scope.labelKey)}</span>
            <span class="block hint">{t(scope.hintKey)}</span>
          </span>
        </label>
      {/each}
    </div>
    {#if store.formBash === 'shared_context'}
      <div class="notice notice-warn">
        <TriangleAlert size={16} strokeWidth={2} class="mt-0.5 shrink-0" />
        {t('instances.bash_shared_warning')}
      </div>
    {:else if store.formBash === 'own_context' && (store.formSessionScope === 'group' || store.formObserveGroup)}
      <div class="notice notice-warn">
        <TriangleAlert size={16} strokeWidth={2} class="mt-0.5 shrink-0" />
        {t('instances.bash_groups_excluded')}
      </div>
    {/if}
    {#if !store.nodeBashEnabled && store.formBash !== 'disabled'}
      <p class="m-0 hint">
        {t('instances.bash_node_off')}
        <button
          type="button"
          class="font-medium text-accent"
          onclick={() => router.navigate('settings', 'bash')}
        >
          {t('instances.bash_open_settings')}
        </button>
      </p>
    {/if}
    {/if}
  </Section>

  <Section collapsible title={t('instances.items_title')} hint={t('instances.items_hint')}>
    {#each kinds as { kind, titleKey } (kind)}
      {@const items = store.itemsOf(kind)}
      <div>
        <h4 class="m-0 mb-2 text-[13px] font-medium text-fg2">{t(titleKey)}</h4>
        {#if items.length === 0}
          <p class="m-0 hint">{t('instances.no_items')}</p>
        {:else}
          <div class="flex flex-col gap-1.5">
            {#each items as item (item.id)}
              <div class="flex items-center gap-3">
                <span class="min-w-0 flex-1 truncate text-[14px] font-medium" title={item.id}>{item.name}</span>
                <Seg
                  size="sm"
                  label={item.name}
                  value={store.policyOf(kind, item.id)}
                  onchange={(next: ItemPolicy) => store.setPolicy(kind, item.id, next)}
                  options={[
                    { value: 'inherit', label: t('instances.policy_inherit') },
                    { value: 'enable', label: t('instances.policy_enable') },
                    { value: 'disable', label: t('instances.policy_disable') },
                  ]}
                />
              </div>
            {/each}
          </div>
        {/if}
      </div>
    {/each}
  </Section>
</div>
