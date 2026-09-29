<script lang="ts">
import {
  AlertCircle,
  Check,
  Lock,
  Pencil,
  Plus,
  RefreshCw,
  Sparkles,
  Trash2,
  X,
} from 'lucide-svelte';
import { t } from '../../stores/i18n.svelte';
import { personasStore } from '../../stores/personas.svelte';
import type { PersonaItem } from '../../types';

/** Draft of the create/edit dialog. `id` is set only while editing an existing persona. */
let draft = $state<{
  id: string | null;
  name: string;
  description: string;
  prompt: string;
}>({ id: null, name: '', description: '', prompt: '' });
let isEditorOpen = $state(false);

let customCount = $derived(
  personasStore.library.filter((persona) => persona.kind === 'custom').length,
);
let canSave = $derived(
  draft.name.trim() !== '' &&
    draft.prompt.trim() !== '' &&
    !personasStore.saving,
);

function openCreate() {
  draft = { id: null, name: '', description: '', prompt: '' };
  personasStore.error = null;
  isEditorOpen = true;
}

function openEdit(persona: PersonaItem) {
  draft = {
    id: persona.id,
    name: persona.name,
    description: persona.description,
    prompt: persona.prompt,
  };
  personasStore.error = null;
  isEditorOpen = true;
}

async function save() {
  if (!canSave) return;
  const body = {
    name: draft.name.trim(),
    description: draft.description.trim(),
    prompt: draft.prompt,
  };
  const ok =
    draft.id === null
      ? await personasStore.create(body)
      : await personasStore.update(draft.id, body);
  if (ok) isEditorOpen = false;
}

async function remove(persona: PersonaItem) {
  if (!confirm(t('personas.delete_confirm', { name: persona.name }))) return;
  await personasStore.remove(persona.id);
}
</script>

<div class="p-6 space-y-6 max-w-7xl mx-auto font-sans">
  <div class="flex flex-wrap items-start justify-between gap-4">
    <div class="max-w-2xl">
      <h3 class="text-base sm:text-lg font-semibold text-zinc-900 dark:text-zinc-100 tracking-tight">{t('personas.heading')}</h3>
      <p class="text-xs sm:text-sm text-zinc-500 mt-1 leading-relaxed">{t('personas.intro')}</p>
    </div>
    <div class="flex items-center gap-2">
      <button
        onclick={() => personasStore.load()}
        class="px-3 py-2 text-sm font-medium rounded-lg bg-white dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700 transition cursor-pointer flex items-center gap-1.5 shadow-2xs"
      >
        <RefreshCw class="w-4 h-4 {personasStore.loading ? 'animate-spin' : ''}" />
        <span>{t('common.refresh')}</span>
      </button>
      <button
        onclick={openCreate}
        class="px-3.5 py-2 bg-indigo-600 hover:bg-indigo-700 text-white rounded-lg text-sm font-medium flex items-center gap-2 transition cursor-pointer shadow-2xs"
      >
        <Plus class="w-4 h-4" />
        <span>{t('personas.add')}</span>
      </button>
    </div>
  </div>

  {#if personasStore.error && !isEditorOpen}
    <p class="text-sm text-rose-600 dark:text-rose-400 flex items-center gap-2">
      <AlertCircle class="w-4 h-4 shrink-0" /> {personasStore.error}
    </p>
  {/if}

  {#if personasStore.catalog === null && personasStore.loading}
    <p class="text-sm text-zinc-400">{t('common.loading')}</p>
  {:else}
    <div class="grid grid-cols-1 lg:grid-cols-2 gap-4">
      {#each personasStore.library as persona (persona.id)}
        <div class="bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl p-4 sm:p-5 shadow-xs space-y-3 flex flex-col">
          <div class="flex items-start justify-between gap-3">
            <div class="min-w-0">
              <div class="flex items-center gap-2 flex-wrap">
                <Sparkles class="w-4 h-4 text-indigo-500 shrink-0" />
                <span class="text-sm sm:text-base font-semibold text-zinc-900 dark:text-zinc-100 truncate">{persona.name}</span>
                {#if persona.kind === 'builtin'}
                  <span class="inline-flex items-center gap-1 px-2 py-0.5 rounded text-[11px] font-mono border border-zinc-200 dark:border-zinc-700 text-zinc-500">
                    <Lock class="w-3 h-3" />
                    {t('personas.builtin')}
                  </span>
                {/if}
              </div>
              <p class="text-[11px] font-mono text-zinc-400 mt-1">{persona.id}</p>
            </div>

            {#if persona.kind === 'custom'}
              <div class="flex items-center gap-1.5 shrink-0">
                <button
                  onclick={() => openEdit(persona)}
                  class="px-2.5 py-1.5 text-xs font-mono bg-zinc-100 dark:bg-zinc-800 hover:bg-zinc-200 dark:hover:bg-zinc-700 text-zinc-700 dark:text-zinc-300 rounded-md transition cursor-pointer flex items-center gap-1.5"
                >
                  <Pencil class="w-3.5 h-3.5" />
                  <span>{t('personas.edit')}</span>
                </button>
                <button
                  onclick={() => remove(persona)}
                  disabled={personasStore.saving || persona.used_by.length > 0}
                  class="p-1.5 text-zinc-400 hover:text-rose-500 transition cursor-pointer disabled:opacity-40 disabled:cursor-not-allowed disabled:hover:text-zinc-400"
                  title={persona.used_by.length > 0
                    ? t('personas.in_use', { instances: persona.used_by.join(', ') })
                    : t('personas.delete')}
                >
                  <Trash2 class="w-4 h-4" />
                </button>
              </div>
            {/if}
          </div>

          {#if persona.description}
            <p class="text-xs sm:text-[13px] text-zinc-500 dark:text-zinc-400">{persona.description}</p>
          {/if}

          <pre class="flex-1 text-xs font-mono leading-relaxed text-zinc-700 dark:text-zinc-300 bg-zinc-50 dark:bg-zinc-950/60 border border-zinc-100 dark:border-zinc-800/80 rounded-lg p-3 whitespace-pre-wrap break-words max-h-40 overflow-y-auto">{persona.prompt}</pre>

          {#if persona.kind === 'builtin'}
            <p class="text-[11px] text-zinc-400">{t('personas.builtin_hint')}</p>
          {/if}
          {#if persona.used_by.length > 0}
            <p class="text-[11px] text-zinc-400 font-mono">{t('personas.in_use', { instances: persona.used_by.join(', ') })}</p>
          {/if}
        </div>
      {/each}
    </div>

    {#if customCount === 0}
      <div class="border border-dashed border-zinc-300 dark:border-zinc-800 rounded-xl p-8 text-center space-y-3">
        <p class="text-sm text-zinc-500">{t('personas.empty')}</p>
        <button
          onclick={openCreate}
          class="px-4 py-2 bg-indigo-600 hover:bg-indigo-700 text-white rounded-lg text-sm font-medium inline-flex items-center gap-2 transition cursor-pointer"
        >
          <Plus class="w-4 h-4" />
          <span>{t('personas.add')}</span>
        </button>
      </div>
    {/if}
  {/if}
</div>

{#if isEditorOpen}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="fixed inset-0 bg-black/40 backdrop-blur-xs z-50 flex items-center justify-center p-4"
    onclick={() => (isEditorOpen = false)}
    role="button"
    tabindex="-1"
  >
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div
      class="w-full max-w-2xl bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl shadow-2xl p-6 space-y-4 max-h-[90vh] overflow-y-auto"
      onclick={(e) => e.stopPropagation()}
      role="dialog"
      tabindex="-1"
    >
      <div class="flex items-center justify-between pb-3 border-b border-zinc-100 dark:border-zinc-800">
        <h3 class="text-base font-semibold text-zinc-900 dark:text-zinc-100 flex items-center gap-2">
          <Sparkles class="w-4.5 h-4.5 text-indigo-500" />
          <span>{draft.id === null ? t('personas.add') : t('personas.edit_title')}</span>
        </h3>
        <button
          onclick={() => (isEditorOpen = false)}
          class="text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200 cursor-pointer"
          aria-label={t('common.cancel')}
        >
          <X class="w-4 h-4" />
        </button>
      </div>

      <div class="space-y-3.5 text-sm">
        <div>
          <label for="persona-name" class="block text-xs text-zinc-500 mb-1">{t('personas.name')}:</label>
          <input
            id="persona-name"
            type="text"
            bind:value={draft.name}
            placeholder={t('personas.name_placeholder')}
            class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
          />
          {#if draft.id !== null}
            <p class="text-[11px] font-mono text-zinc-400 mt-1">id: {draft.id}</p>
          {/if}
        </div>

        <div>
          <label for="persona-description" class="block text-xs text-zinc-500 mb-1">{t('personas.description')} ({t('models.optional')}):</label>
          <input
            id="persona-description"
            type="text"
            bind:value={draft.description}
            class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
          />
        </div>

        <div>
          <label for="persona-prompt" class="block text-xs text-zinc-500 mb-1">{t('personas.prompt')}:</label>
          <textarea
            id="persona-prompt"
            bind:value={draft.prompt}
            rows="10"
            placeholder={t('personas.prompt_placeholder')}
            class="w-full px-3.5 py-2 text-sm font-mono leading-relaxed bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden resize-y"
          ></textarea>
          <p class="text-[11px] text-zinc-400 mt-1.5 leading-relaxed">{t('personas.prompt_hint')}</p>
        </div>

        {#if personasStore.error}
          <p class="text-xs text-rose-600 dark:text-rose-400 flex items-center gap-1.5">
            <AlertCircle class="w-3.5 h-3.5 shrink-0" /> {personasStore.error}
          </p>
        {/if}
      </div>

      <div class="pt-3 border-t border-zinc-100 dark:border-zinc-800 flex items-center justify-end gap-2">
        <button
          onclick={() => (isEditorOpen = false)}
          class="px-3.5 py-2 bg-zinc-100 dark:bg-zinc-800 hover:bg-zinc-200 dark:hover:bg-zinc-700 text-zinc-700 dark:text-zinc-300 rounded-lg text-sm font-medium cursor-pointer"
        >
          {t('common.cancel')}
        </button>
        <button
          onclick={save}
          disabled={!canSave}
          class="px-4 py-2 bg-indigo-600 hover:bg-indigo-700 text-white rounded-lg text-sm font-medium cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2"
        >
          {#if personasStore.saving}
            <RefreshCw class="w-4 h-4 animate-spin" />
          {:else}
            <Check class="w-4 h-4" />
          {/if}
          <span>{t('personas.save')}</span>
        </button>
      </div>
    </div>
  </div>
{/if}
