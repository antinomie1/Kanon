<script lang="ts">
import {
  AlertTriangle,
  Boxes,
  ChevronDown,
  ChevronRight,
  RefreshCw,
  Search,
  Wrench,
} from 'lucide-svelte';
import { api } from '../../api/client';
import { t } from '../../stores/i18n.svelte';
import type { ToolItem, ToolSource } from '../../types';

/**
 * Tool catalog console.
 *
 * Shows every tool the model can actually call right now and who provides it: native builtins,
 * running plugins, and enabled MCP servers. A provider that is switched off contributes nothing
 * here, which is exactly what makes this list trustworthy as a "what can the model do" view.
 */
let catalog = $state<{
  total: number;
  builtin: number;
  plugin: number;
  mcp: number;
} | null>(null);
let tools = $state<ToolItem[]>([]);
let loading = $state(true);
let error = $state<string | null>(null);

let search = $state('');
let sourceFilter = $state<'all' | ToolSource>('all');
/** Names whose parameter schema is expanded; the raw JSON is only interesting on demand. */
let expanded = $state<Record<string, boolean>>({});

async function loadData() {
  loading = true;
  error = null;
  try {
    const res = await api.getTools();
    catalog = {
      total: res.total,
      builtin: res.builtin,
      plugin: res.plugin,
      mcp: res.mcp,
    };
    tools = res.tools;
  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
  } finally {
    loading = false;
  }
}

/** Tools matching the current search text and source filter. */
function visibleTools(): ToolItem[] {
  const needle = search.trim().toLowerCase();
  return tools.filter((tool) => {
    if (sourceFilter !== 'all' && tool.source !== sourceFilter) return false;
    if (!needle) return true;
    return (
      tool.name.toLowerCase().includes(needle) ||
      tool.description.toLowerCase().includes(needle) ||
      tool.provider_id.toLowerCase().includes(needle)
    );
  });
}

/** Badge class per source. */
function sourceClass(source: ToolSource): string {
  switch (source) {
    case 'builtin':
      return 'border-indigo-200 dark:border-indigo-800/60 text-indigo-600 dark:text-indigo-400 bg-indigo-50 dark:bg-indigo-950/60';
    case 'plugin':
      return 'border-emerald-200 dark:border-emerald-800/60 text-emerald-600 dark:text-emerald-400 bg-emerald-50 dark:bg-emerald-950/60';
    case 'mcp':
      return 'border-violet-200 dark:border-violet-800/60 text-violet-600 dark:text-violet-400 bg-violet-50 dark:bg-violet-950/60';
  }
}

function toggleParameters(name: string) {
  expanded = { ...expanded, [name]: !expanded[name] };
}

$effect(() => {
  void loadData();
});
</script>

<div class="space-y-6">
  <div class="flex items-center justify-between">
    <div>
      <h3 class="text-base sm:text-lg font-semibold text-zinc-900 dark:text-zinc-100 tracking-tight">
        {t('tools.title')}
      </h3>
      <p class="text-xs sm:text-sm text-zinc-500">{t('tools.subtitle')}</p>
    </div>
    <button
      onclick={loadData}
      class="px-3 py-1.5 text-xs sm:text-sm font-medium rounded-lg bg-white dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700 transition cursor-pointer flex items-center gap-1.5 shadow-2xs"
    >
      <RefreshCw class="w-4 h-4" />
      <span>{t('common.refresh')}</span>
    </button>
  </div>

  {#if error}
    <p class="text-sm text-rose-600 dark:text-rose-400 flex items-center gap-2">
      <AlertTriangle class="w-4 h-4" /> {error}
    </p>
  {/if}

  {#if catalog}
    <div class="grid grid-cols-2 sm:grid-cols-4 gap-3">
      {#each [
        { key: 'all', label: t('tools.total'), value: catalog.total },
        { key: 'builtin', label: t('tools.source_builtin'), value: catalog.builtin },
        { key: 'plugin', label: t('tools.source_plugin'), value: catalog.plugin },
        { key: 'mcp', label: t('tools.source_mcp'), value: catalog.mcp },
      ] as card (card.key)}
        <button
          type="button"
          onclick={() => (sourceFilter = card.key as 'all' | ToolSource)}
          class="text-left rounded-xl border p-3.5 transition cursor-pointer
            {sourceFilter === card.key
            ? 'border-indigo-400 dark:border-indigo-600 bg-indigo-50/60 dark:bg-indigo-950/30'
            : 'border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 hover:border-zinc-300 dark:hover:border-zinc-700'}"
        >
          <div class="text-xs text-zinc-500">{card.label}</div>
          <div class="text-xl font-semibold text-zinc-900 dark:text-zinc-100 mt-0.5">
            {card.value}
          </div>
        </button>
      {/each}
    </div>

    <label class="flex items-center gap-2 px-3 py-2 rounded-lg bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-700">
      <Search class="w-4 h-4 text-zinc-400 shrink-0" />
      <input
        bind:value={search}
        placeholder={t('tools.search_placeholder')}
        class="w-full bg-transparent text-sm focus:outline-hidden"
      />
    </label>
  {/if}

  {#if loading}
    <div class="p-12 text-center text-sm text-zinc-400">{t('common.loading')}</div>
  {:else if tools.length === 0}
    <div
      class="p-8 rounded-xl border border-dashed border-zinc-300 dark:border-zinc-800 text-center text-zinc-400 text-sm space-y-2"
    >
      <Wrench class="w-6 h-6 mx-auto stroke-[1.5]" />
      <p>{t('tools.empty')}</p>
      <p class="text-xs">{t('tools.empty_hint')}</p>
    </div>
  {:else}
    {@const shown = visibleTools()}
    {#if shown.length === 0}
      <div
        class="p-8 rounded-xl border border-dashed border-zinc-300 dark:border-zinc-800 text-center text-zinc-400 text-sm"
      >
        {t('tools.no_match')}
      </div>
    {:else}
      <div class="grid grid-cols-1 gap-3">
        {#each shown as tool (tool.name)}
          <div
            class="rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 shadow-2xs"
          >
            <div class="p-4 flex items-start justify-between gap-4">
              <div class="flex items-start gap-3 min-w-0">
                <div class="p-2 rounded-lg bg-zinc-100 dark:bg-zinc-800 text-zinc-500 shrink-0">
                  <Wrench class="w-4 h-4" />
                </div>
                <div class="min-w-0">
                  <div class="flex items-center gap-2 flex-wrap">
                    <code class="text-sm font-mono font-medium text-zinc-900 dark:text-zinc-100">
                      {tool.name}
                    </code>
                    <span class="px-2 py-0.5 rounded-md text-[11px] font-medium border {sourceClass(tool.source)}">
                      {tool.source === 'builtin'
                        ? t('tools.source_builtin')
                        : tool.source === 'plugin'
                          ? t('tools.source_plugin')
                          : t('tools.source_mcp')}
                    </span>
                  </div>
                  <p class="text-xs text-zinc-500 mt-1">{tool.description}</p>
                  <div class="flex items-center gap-3 mt-1.5 text-[11px] font-mono text-zinc-400">
                    <span>{tool.provider_id}</span>
                    {#if tool.host_id}
                      <span class="flex items-center gap-1">
                        <Boxes class="w-3 h-3" />
                        {tool.host_id}
                      </span>
                    {/if}
                  </div>
                </div>
              </div>

              <button
                type="button"
                onclick={() => toggleParameters(tool.name)}
                class="shrink-0 px-2.5 py-1 text-[11px] rounded-lg border border-zinc-200 dark:border-zinc-700 text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200 transition cursor-pointer flex items-center gap-1"
              >
                {#if expanded[tool.name]}
                  <ChevronDown class="w-3 h-3" />
                {:else}
                  <ChevronRight class="w-3 h-3" />
                {/if}
                {t('tools.parameters')}
              </button>
            </div>

            {#if expanded[tool.name]}
              <pre
                class="mx-4 mb-4 p-3 rounded-lg bg-zinc-50 dark:bg-zinc-950/60 border border-zinc-200 dark:border-zinc-800 text-[11px] font-mono text-zinc-600 dark:text-zinc-300 overflow-x-auto">{JSON.stringify(
                  tool.parameters,
                  null,
                  2,
                )}</pre>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  {/if}
</div>
