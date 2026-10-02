<script lang="ts">
import { agentName, agentsStore } from '../../stores/agents.svelte';
import { t } from '../../stores/i18n.svelte';
import { toasts } from '../../stores/toast.svelte';
import Section from '../ui/Section.svelte';
import Select from '../ui/Select.svelte';

/**
 * The node's default agent: the engine that answers for every instance without its own choice.
 *
 * A change applies at once, like the default model. Only the built-in agent exists today, so the
 * picker mostly shows which engine answers; the list comes from the node.
 */

$effect(() => {
  void agentsStore.load();
});

/** Picker value; kept in sync with the node so a refused change snaps back. */
let chosen = $state('');
$effect(() => {
  chosen = agentsStore.defaultAgent ?? '';
});

/** Takes the picked value from the event, not `chosen`, so listener order cannot matter. */
async function apply(next: string) {
  if (await agentsStore.setDefault(next)) {
    toasts.ok(t('settings.saved_toast'));
  } else {
    toasts.error(agentsStore.error ?? t('common.error'));
    chosen = agentsStore.defaultAgent ?? '';
  }
}
</script>

<Section title={t('agents.title')} hint={t('agents.hint')}>
  {#if !agentsStore.catalog}
    <p class="m-0 hint">{agentsStore.error ?? t('common.loading')}</p>
  {:else}
    <label class="sr-only" for="default-agent">{t('agents.title')}</label>
    <Select
      id="default-agent"
      bind:value={chosen}
      disabled={agentsStore.saving || agentsStore.agents.length < 2}
      onchange={(e) => void apply(e.currentTarget.value)}
    >
      {#each agentsStore.agents as agent (agent)}
        <option value={agent}>{agentName(agent)}</option>
      {/each}
    </Select>
    {#if chosen === 'builtin'}
      <p class="m-0 hint">{t('agents.builtin_hint')}</p>
    {/if}
  {/if}
</Section>
