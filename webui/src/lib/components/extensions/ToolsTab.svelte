<script lang="ts">
import { ChevronDown, RefreshCw, Search, Wrench } from 'lucide-svelte';
import { untrack } from 'svelte';
import { api } from '../../api/client';
import { errorText } from '../../format';
import { t } from '../../stores/i18n.svelte';
import type { ToolCatalog, ToolItem, ToolSource } from '../../types';
import EmptyState from '../ui/EmptyState.svelte';
import Seg from '../ui/Seg.svelte';

/**
 * Every tool the model can call right now, and who provides it.
 *
 * A provider that is switched off contributes nothing here, which is what makes the list
 * trustworthy as the answer to "what can the model do". Parameter schemas are shown on demand.
 */

let catalog = $state<ToolCatalog | null>(null);
let loading = $state(false);
let error = $state<string | null>(null);
let search = $state('');
let source = $state<'all' | ToolSource>('all');
let expanded = $state<Record<string, boolean>>({});

async function load() {
  loading = true;
  error = null;
  try {
    catalog = await api.getTools();
  } catch (e) {
    error = errorText(e);
  } finally {
    loading = false;
  }
}

$effect(() => {
  untrack(() => void load());
});

const shown = $derived.by<ToolItem[]>(() => {
  const needle = search.trim().toLowerCase();
  return (catalog?.tools ?? []).filter((tool) => {
    if (source !== 'all' && tool.source !== source) return false;
    if (!needle) return true;
    return (
      tool.name.toLowerCase().includes(needle) ||
      tool.description.toLowerCase().includes(needle) ||
      tool.provider_id.toLowerCase().includes(needle)
    );
  });
});

const SOURCE_KEY: Record<ToolSource, string> = {
  builtin: 'extensions.source_builtin',
  plugin: 'extensions.source_plugin',
  mcp: 'extensions.source_mcp',
};
</script>

<div class="flex flex-wrap items-center justify-between gap-3 px-1">
  <p class="m-0 max-w-[68ch] hint">{t('extensions.tools_hint')}</p>
  <button type="button" class="btn" disabled={loading} onclick={() => void load()}>
    <RefreshCw size={16} strokeWidth={2} class={loading ? 'animate-spin' : ''} />
    {t('platforms.refresh')}
  </button>
</div>

{#if error}
  <div class="notice notice-bad">{error}</div>
{/if}

{#if catalog && catalog.tools.length === 0}
  <div class="card">
    <EmptyState icon={Wrench} title={t('extensions.tools_empty')} text={t('extensions.tools_empty_text')} />
  </div>
{:else if catalog}
  <section class="card overflow-hidden">
    <div class="flex flex-wrap items-center gap-3 border-b border-line px-5 py-3.5">
      <Seg
        size="sm"
        label={t('extensions.source')}
        value={source}
        onchange={(next: 'all' | ToolSource) => (source = next)}
        options={[
          { value: 'all', label: t('extensions.source_all', { n: catalog.total }) },
          { value: 'builtin', label: `${t('extensions.source_builtin')} ${catalog.builtin}` },
          { value: 'plugin', label: `${t('extensions.source_plugin')} ${catalog.plugin}` },
          { value: 'mcp', label: `${t('extensions.source_mcp')} ${catalog.mcp}` },
        ]}
      />
      <label class="relative ml-auto w-full sm:w-[280px]">
        <Search size={16} strokeWidth={2} class="pointer-events-none absolute top-1/2 left-3.5 -translate-y-1/2 text-fg3" />
        <input
          class="input input-search h-8!"
          type="search"
          aria-label={t('extensions.tools_search')}
          placeholder={t('extensions.tools_search')}
          bind:value={search}
        />
      </label>
    </div>

    {#if shown.length === 0}
      <p class="m-0 px-5 py-8 text-center hint">{t('extensions.tools_no_match')}</p>
    {:else}
      <ul class="m-0 list-none p-0">
        {#each shown as tool (tool.name)}
          {@const open = expanded[tool.name] ?? false}
          <li class="border-t border-line px-5 py-3.5 first:border-t-0">
            <div class="flex items-start gap-4">
              <div class="min-w-0 flex-1">
                <div class="flex flex-wrap items-center gap-x-2.5 gap-y-1">
                  <code class="text-[14px] font-medium">{tool.name}</code>
                  <span class="chip chip-sm {tool.source === 'builtin' ? 'chip-accent' : 'chip-muted'}">
                    {t(SOURCE_KEY[tool.source])}
                  </span>
                  {#if tool.source !== 'builtin'}
                    <span class="text-[12.5px] text-fg3">{tool.provider_id}</span>
                  {/if}
                </div>
                {#if tool.description}
                  <p class="m-0 mt-1 max-w-[80ch] text-[13.5px] text-fg2">{tool.description}</p>
                {/if}
              </div>
              <button
                type="button"
                class="btn btn-quiet btn-xs"
                aria-expanded={open}
                onclick={() => (expanded = { ...expanded, [tool.name]: !open })}
              >
                {t('extensions.parameters')}
                <ChevronDown size={14} strokeWidth={2.2} class="transition-transform {open ? 'rotate-180' : ''}" />
              </button>
            </div>
            {#if open}
              <pre class="scroll-thin m-0 mt-3 overflow-x-auto rounded-xl bg-sunk p-3.5 text-[12.5px]">{JSON.stringify(tool.parameters, null, 2)}</pre>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  </section>
{:else if !error}
  <p class="m-0 px-1 hint">{t('common.loading')}</p>
{/if}
