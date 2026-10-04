<script lang="ts">
import { onMount } from 'svelte';
import { api } from '../../api/client';
import { errorText } from '../../format';
import { t } from '../../stores/i18n.svelte';
import type { DshModels } from '../../types';
import Select from '../ui/Select.svelte';

let { value = $bindable(''), available = $bindable(false), disabled = false }:
  { value?: string; available?: boolean; disabled?: boolean } = $props();
let catalog = $state<DshModels | null>(null);
let error = $state<string | null>(null);
const defaultModel = $derived(catalog ? `${catalog.default.provider}/${catalog.default.model}` : '');

onMount(() => {
  let disposed = false;
  void api.getDshModels().then(result => {
    if (disposed) return;
    catalog = result;
    available = true;
  }).catch(e => { if (!disposed) error = errorText(e); });
  return () => { disposed = true; };
});
</script>

<Select id="dsh-model" bind:value disabled={disabled || !catalog} aria-label={t('chat.model')}>
  <option value="">{catalog ? t('dsh.session_model') + ' (' + defaultModel + ')' : t('common.loading')}</option>
  {#if value && !catalog?.groups.some(group => group.models.some(model => `${group.id}/${model.id}` === value))}
    <option value={value}>{value}</option>
  {/if}
  {#each catalog?.groups ?? [] as group (group.id)}
    <optgroup label={group.name}>
      {#each group.models as model (model.id)}
        <option value={`${group.id}/${model.id}`}>{model.name}</option>
      {/each}
    </optgroup>
  {/each}
</Select>
{#if error}<p class="notice notice-bad">{error}</p>{/if}
{#each catalog?.failures ?? [] as failure (failure.id)}
  <p class="notice notice-warn">{failure.name}: {failure.message}</p>
{/each}
