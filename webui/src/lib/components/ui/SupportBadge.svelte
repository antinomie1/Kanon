<script lang="ts">
import { capabilityStore } from '../../stores/capabilities.svelte';
import { t } from '../../stores/i18n.svelte';
import type { Capability } from '../../types';

/**
 * Names the adapters a setting affects: those declaring any of `capabilities`.
 *
 * A setting no registered adapter supports is still shown, but flagged, so an operator is not left
 * wondering why switching it on changes nothing.
 */
let { capabilities }: { capabilities: Capability[] } = $props();

$effect(() => {
  void capabilityStore.ensureLoaded();
});

const names = $derived(capabilityStore.supporters(capabilities));
</script>

{#if capabilityStore.loaded}
  {#if names.length > 0}
    <span class="block text-[11px] text-zinc-400 mt-0.5">
      {t('capability.supported_by')}: {names.join(' · ')}
    </span>
  {:else}
    <span class="block text-[11px] text-amber-600 dark:text-amber-400 mt-0.5">
      {t('capability.none')}
    </span>
  {/if}
{/if}
