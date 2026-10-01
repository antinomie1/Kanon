<script lang="ts">
import {
  type CommandPolicyDraft,
  commandPolicyOfDraft,
  commandPolicyStore,
  draftOfCommandPolicy,
} from '../../stores/commandPolicy.svelte';
import { t } from '../../stores/i18n.svelte';
import { toasts } from '../../stores/toast.svelte';
import CommandPolicyEditor from '../ui/CommandPolicyEditor.svelte';
import Section from '../ui/Section.svelte';

let draft = $state<CommandPolicyDraft | null>(null);

$effect(() => {
  void commandPolicyStore.load().then(() => {
    if (commandPolicyStore.policy)
      draft = draftOfCommandPolicy(commandPolicyStore.policy);
  });
});

const dirty = $derived(
  draft !== null &&
    commandPolicyStore.policy !== null &&
    JSON.stringify(commandPolicyOfDraft(draft)) !==
      JSON.stringify(
        commandPolicyOfDraft(draftOfCommandPolicy(commandPolicyStore.policy)),
      ),
);

/** Saves the draft; the node normalizes it and the form adopts what it enforces. */
async function save() {
  if (!draft) return;
  if (await commandPolicyStore.save(commandPolicyOfDraft(draft))) {
    if (commandPolicyStore.policy)
      draft = draftOfCommandPolicy(commandPolicyStore.policy);
    toasts.ok(t('settings.saved_toast'));
  } else {
    toasts.error(commandPolicyStore.error ?? t('common.error'));
  }
}
</script>

<Section title={t('commands.title')} hint={t('commands.hint')}>
  {#if !draft}
    <p class="m-0 hint">{commandPolicyStore.error ?? t('common.loading')}</p>
  {:else}
    <CommandPolicyEditor bind:draft />
  {/if}
</Section>

{#if draft}
  <div class="flex justify-end gap-2.5 border-t border-line py-5">
    <button
      type="button"
      class="btn"
      disabled={!dirty || commandPolicyStore.saving}
      onclick={() => commandPolicyStore.policy && (draft = draftOfCommandPolicy(commandPolicyStore.policy))}
    >
      {t('instances.discard')}
    </button>
    <button type="button" class="btn btn-primary" disabled={!dirty || commandPolicyStore.saving} onclick={save}>
      {commandPolicyStore.saving ? t('instances.saving') : t('instances.save_changes')}
    </button>
  </div>
{/if}
