<script lang="ts">
import { MessageCircle, Plus, Trash2 } from 'lucide-svelte';
import { chatStore } from '../../stores/chat.svelte';
import { confirmDialog } from '../../stores/confirm.svelte';
import { i18n, t } from '../../stores/i18n.svelte';
import { instancesStore } from '../../stores/instances.svelte';
import { personasStore } from '../../stores/personas.svelte';
import { router } from '../../stores/router.svelte';
import { toasts } from '../../stores/toast.svelte';
import type { PersonaItem } from '../../types';
import Button from '../ui/Button.svelte';
import Modal from '../ui/Modal.svelte';
import PageHead from '../ui/PageHead.svelte';
import TextField from '../ui/TextField.svelte';

/**
 * Persona library: the instruction text placed at the top of every request, which decides who the
 * model is and how it speaks.
 *
 * The base assistant ships with the node and is read-only; everything else belongs to the
 * operator. A persona an instance uses cannot be deleted, so the card says which instances do.
 */

/** Draft of the editor; `id` is set only while editing an existing persona. */
let draft = $state<{
  id: string | null;
  name: string;
  description: string;
  prompt: string;
}>({
  id: null,
  name: '',
  description: '',
  prompt: '',
});
let editorOpen = $state(false);
/** Personas whose whole prompt is shown instead of the first lines. */
let expanded = $state<Record<string, boolean>>({});

const loaded = $derived(personasStore.catalog !== null);
const customCount = $derived(
  personasStore.library.filter((p) => p.kind === 'custom').length,
);
const canSave = $derived(
  draft.name.trim() !== '' &&
    draft.prompt.trim() !== '' &&
    !personasStore.saving,
);

function openCreate() {
  draft = { id: null, name: '', description: '', prompt: '' };
  personasStore.error = null;
  editorOpen = true;
}

function openEdit(persona: PersonaItem) {
  draft = {
    id: persona.id,
    name: persona.name,
    description: persona.description,
    prompt: persona.prompt,
  };
  personasStore.error = null;
  editorOpen = true;
}

async function save() {
  if (!canSave) return;
  const body = {
    name: draft.name.trim(),
    description: draft.description.trim(),
    // Sent exactly as typed: the prompt is the cached prefix of every request.
    prompt: draft.prompt,
  };
  const creating = draft.id === null;
  const ok = creating
    ? await personasStore.create(body)
    : await personasStore.update(draft.id as string, body);
  if (ok) {
    editorOpen = false;
    toasts.ok(
      t(creating ? 'personas.created_toast' : 'personas.saved_toast', {
        name: body.name,
      }),
    );
  }
}

async function remove(persona: PersonaItem) {
  const yes = await confirmDialog({
    title: t('personas.delete_title', { name: persona.name }),
    message: t('personas.delete_text'),
    confirm: t('personas.delete'),
    danger: true,
  });
  if (!yes) return;
  if (await personasStore.remove(persona.id)) {
    toasts.ok(t('personas.deleted_toast', { name: persona.name }));
  } else {
    toasts.error(personasStore.error ?? t('common.error'));
  }
}

/** Opens the test chat answering with this persona. */
function tryOut(persona: PersonaItem) {
  chatStore.choose(
    persona.id === personasStore.baseId ? '' : `p:${persona.id}`,
  );
  router.navigate('chat');
}

function usedBy(persona: PersonaItem): string {
  const names = persona.used_by.map(
    (id) => instancesStore.find(id)?.name ?? id,
  );
  return names.join(i18n.locale === 'zh' ? '、' : ', ');
}

/** Long prompts start folded; a few lines are enough to recognise one. */
function isLong(prompt: string): boolean {
  return prompt.length > 240 || prompt.split('\n').length > 4;
}
</script>

<PageHead title={t('nav.personas')}>
  {#snippet sub()}
    <span class="max-w-[72ch]">{t('personas.sub')}</span>
  {/snippet}
  {#snippet actions()}
    <Button type="button" variant="filled" onclick={openCreate}>
      <Plus size={16} strokeWidth={2.2} />
      {t('personas.add')}
    </Button>
  {/snippet}
</PageHead>

{#if personasStore.error && !editorOpen}
  <div class="notice notice-bad">{personasStore.error}</div>
{/if}

{#if !loaded}
  {#if !personasStore.error}<p class="m-0 px-1 hint">{t('common.loading')}</p>{/if}
{:else}
  <div class="flex flex-col gap-3">
    {#each personasStore.library as persona (persona.id)}
      {@const builtin = persona.kind === 'builtin'}
      {@const inUse = persona.used_by.length > 0}
      {@const open = expanded[persona.id] ?? false}
      <article class="card flex flex-col gap-3 px-[22px] py-[18px]">
        <div class="flex flex-wrap items-start gap-x-6 gap-y-2">
          <div class="min-w-0 flex-1 basis-[300px]">
            <div class="flex flex-wrap items-center gap-x-2.5 gap-y-1">
              <h2 class="m-0 text-[17px] font-semibold">{persona.name}</h2>
              {#if builtin}<span class="chip chip-sm chip-muted">{t('personas.builtin')}</span>{/if}
            </div>
            {#if persona.description}
              <p class="m-0 mt-0.5 max-w-[72ch] text-[14px] text-fg2">{persona.description}</p>
            {/if}
          </div>
          <div class="ml-auto flex items-center gap-2">
            <Button type="button" variant="text" size="sm" onclick={() => tryOut(persona)}>
              <MessageCircle size={15} strokeWidth={2} />
              {t('personas.try')}
            </Button>
            {#if !builtin}
              <Button type="button" size="sm" onclick={() => openEdit(persona)}>
                {t('personas.edit')}
              </Button>
              <Button
                type="button"
                variant="text" size="sm" square
                aria-label={t('personas.delete_title', { name: persona.name })}
                title={inUse ? t('personas.in_use_block') : t('personas.delete')}
                disabled={inUse || personasStore.saving}
                onclick={() => void remove(persona)}
              >
                <Trash2 size={16} strokeWidth={2} />
              </Button>
            {/if}
          </div>
        </div>

        <div class="rounded-xl bg-sunk px-4 py-3">
          <p
            class="m-0 text-[14px] leading-relaxed break-words whitespace-pre-wrap text-fg2 {open
              ? ''
              : 'line-clamp-3'}"
          >
            {persona.prompt}
          </p>
          {#if isLong(persona.prompt)}
            <button
              type="button"
              class="mt-1.5 cursor-pointer text-[13px] font-medium text-accent hover:underline"
              aria-expanded={open}
              onclick={() => (expanded = { ...expanded, [persona.id]: !open })}
            >
              {open ? t('personas.fold') : t('personas.unfold')}
            </button>
          {/if}
        </div>

        <p class="m-0 flex flex-wrap gap-x-4 text-[12.5px] text-fg3">
          <span class="font-mono">{persona.id}</span>
          {#if builtin}
            <span>{t('personas.builtin_hint')}</span>
          {/if}
          {#if inUse}
            <span>{t('personas.used_by', { names: usedBy(persona) })}</span>
          {/if}
        </p>
      </article>
    {/each}

    {#if customCount === 0}
      <div class="card flex flex-wrap items-center justify-between gap-3 px-[22px] py-4">
        <p class="m-0 text-[14px] text-fg2">{t('personas.empty')}</p>
        <Button type="button" size="sm" onclick={openCreate}>
          <Plus size={15} strokeWidth={2.2} />
          {t('personas.add')}
        </Button>
      </div>
    {/if}
  </div>
{/if}

<Modal
  open={editorOpen}
  title={draft.id === null ? t('personas.add') : t('personas.edit_title', { name: draft.name || draft.id })}
  width="max-w-2xl"
  locked={personasStore.saving}
  onclose={() => (editorOpen = false)}
>
  <form
    id="persona-editor"
    class="flex flex-col gap-4"
    onsubmit={(e) => {
      e.preventDefault();
      void save();
    }}
  >
    <div class="grid gap-4 sm:grid-cols-2">
      <div>
        <label class="label" for="persona-name">{t('personas.name')}</label>
        <TextField id="persona-name" placeholder={t('personas.name_placeholder')} bind:value={draft.name} />
      </div>
      <div>
        <label class="label" for="persona-description">{t('personas.description')}</label>
        <TextField
          id="persona-description"
         
          placeholder={t('llm.optional')}
          bind:value={draft.description}
        />
      </div>
    </div>
    <div>
      <label class="label" for="persona-prompt">{t('personas.prompt')}</label>
      <textarea
        id="persona-prompt"
        class="input min-h-[260px] resize-y py-3 leading-relaxed"
        placeholder={t('personas.prompt_placeholder')}
        bind:value={draft.prompt}
      ></textarea>
      <p class="m-0 mt-2 hint">{t('personas.prompt_hint')}</p>
    </div>
    {#if personasStore.error}
      <div class="notice notice-bad" role="alert"><span class="min-w-0 break-words">{personasStore.error}</span></div>
    {/if}
  </form>

  {#snippet footer()}
    <Button type="button" disabled={personasStore.saving} onclick={() => (editorOpen = false)}>
      {t('common.cancel')}
    </Button>
    <Button type="submit" form="persona-editor" variant="filled" disabled={!canSave}>
      {personasStore.saving
        ? t('instances.saving')
        : draft.id === null
          ? t('personas.create')
          : t('instances.save_changes')}
    </Button>
  {/snippet}
</Modal>
