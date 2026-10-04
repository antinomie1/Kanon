<script lang="ts">
import { MessagesSquare, RefreshCw, Search } from 'lucide-svelte';
import { untrack } from 'svelte';
import { api } from '../../api/client';
import { errorText, formatDuration } from '../../format';
import { confirmDialog } from '../../stores/confirm.svelte';
import { t } from '../../stores/i18n.svelte';
import { instancesStore } from '../../stores/instances.svelte';
import { personasStore } from '../../stores/personas.svelte';
import { toasts } from '../../stores/toast.svelte';
import type { SessionSummary } from '../../types';
import Button from '../ui/Button.svelte';
import EmptyState from '../ui/EmptyState.svelte';
import Modal from '../ui/Modal.svelte';
import PageHead from '../ui/PageHead.svelte';
import Select from '../ui/Select.svelte';
import TextField from '../ui/TextField.svelte';

/**
 * Conversations the node remembers, most recently active first.
 *
 * Session keys are made for machines (`instance:<id>:<channel>:<sender>#<generation>`), so each
 * row names the instance and conversation in words and keeps the raw key underneath for whoever
 * needs to match it against a log line.
 */

let sessions = $state<SessionSummary[]>([]);
let total = $state(0);
let loaded = $state(false);
let loading = $state(false);
let error = $state<string | null>(null);
let search = $state('');
/** Sessions with a request in flight. */
let busy = $state<Record<string, boolean>>({});

/** Session whose persona is being chosen, with the choice so far. */
let binding = $state<{
  key: string;
  name: string;
  persona: string;
  /** Instance settings take precedence while preserving the saved session choice. */
  pinned: boolean;
} | null>(null);
let bindError = $state<string | null>(null);
let bindSaving = $state(false);

async function load() {
  loading = true;
  error = null;
  try {
    const res = await api.getSessions(search.trim());
    sessions = res.items;
    total = res.total;
  } catch (e) {
    error = errorText(e);
  } finally {
    loading = false;
    loaded = true;
  }
}

// Reload as the search changes, a moment after typing stops, so each keystroke is not a request.
$effect(() => {
  void search;
  const timer = window.setTimeout(
    () => untrack(() => void load()),
    loaded ? 250 : 0,
  );
  return () => window.clearTimeout(timer);
});

interface Parsed {
  /** Who the conversation is with, in words. */
  name: string;
  /** Instance that owns the conversation, when it is one. */
  instanceId: string | null;
  /** Channel and sender the conversation belongs to, when the key says. */
  conversation: string | null;
  /** How many times `/new` started this conversation over. */
  generation: number;
}

function parse(key: string): Parsed {
  const match = /^instance:([^:]+):(.*?)(?:#(\d+))?$/.exec(key);
  if (match) {
    const [, id, conversation, generation] = match;
    return {
      name:
        instancesStore.find(id)?.name ?? t('sessions.gone_instance', { id }),
      instanceId: id,
      conversation: conversation || null,
      generation: Number(generation ?? 0),
    };
  }
  if (key === 'webui:chat' || key.startsWith('webui:chat:')) {
    return {
      name: t('nav.chat'),
      instanceId: null,
      conversation: null,
      generation: 0,
    };
  }
  return { name: key, instanceId: null, conversation: null, generation: 0 };
}

/**
 * Names the saved session choice, which instance settings may take precedence over.
 * A legacy `instance:<id>` binding repeats the instance name, so label it as its own prompt.
 */
function personaName(id: string, instanceId: string | null): string {
  if (instanceId !== null && id === `instance:${instanceId}`) {
    return t('sessions.own_prompt');
  }
  return personasStore.all.find((persona) => persona.id === id)?.name ?? id;
}

function ago(seconds: number): string {
  return t('sessions.active_ago', {
    time: formatDuration(Date.now() / 1000 - seconds),
  });
}

async function reset(session: SessionSummary, name: string) {
  const yes = await confirmDialog({
    title: t('sessions.reset_title', { name }),
    message: t('sessions.reset_text'),
    confirm: t('sessions.reset'),
    danger: true,
  });
  if (!yes) return;
  const key = session.session_key;
  busy = { ...busy, [key]: true };
  try {
    await api.resetSession(key);
    toasts.ok(t('sessions.reset_toast', { name }));
    await load();
  } catch (e) {
    toasts.error(t('sessions.reset_failed', { error: errorText(e) }));
  } finally {
    busy = { ...busy, [key]: false };
  }
}

function openPersona(session: SessionSummary, info: Parsed) {
  const owner = info.instanceId
    ? instancesStore.find(info.instanceId)
    : undefined;
  binding = {
    key: session.session_key,
    name: info.name,
    persona: session.persona_id ?? '',
    pinned: Boolean(owner?.system_prompt?.trim() || owner?.persona_id),
  };
  bindError = null;
}

async function applyPersona() {
  if (!binding) return;
  bindSaving = true;
  bindError = null;
  try {
    // Clear only the session choice; instance settings still take precedence over the base.
    await api.setSessionPersona(binding.key, binding.persona || null);
    toasts.ok(t('sessions.persona_toast', { name: binding.name }));
    binding = null;
    await load();
  } catch (e) {
    bindError = errorText(e);
  } finally {
    bindSaving = false;
  }
}
</script>

<PageHead title={t('nav.sessions')}>
  {#snippet sub()}
    <span>{t('sessions.sub')}</span>
  {/snippet}
  {#snippet actions()}
    <Button type="button" disabled={loading} onclick={() => void load()}>
      <RefreshCw size={16} strokeWidth={2} class={loading ? 'animate-spin' : ''} />
      {t('platforms.refresh')}
    </Button>
  {/snippet}
</PageHead>

<div class="flex flex-wrap items-center justify-between gap-3">
  <div class="min-w-[200px] flex-1 sm:max-w-[360px]">
    <TextField
      pill
      type="search"
      aria-label={t('common.search')}
      placeholder={t('sessions.search')}
      bind:value={search}
    >
      {#snippet leading()}<Search size={16} strokeWidth={2} class="text-fg3" />{/snippet}
    </TextField>
  </div>
  {#if loaded && !error}
    <span class="px-1 text-[13.5px] text-fg2">
      {total > sessions.length
        ? t('sessions.showing', { shown: sessions.length, total })
        : total === 1
          ? t('sessions.count_one')
          : t('sessions.count', { n: total })}
    </span>
  {/if}
</div>

{#if error}
  <div class="notice notice-bad">{error}</div>
{/if}

{#if !loaded}
  <p class="m-0 px-1 hint">{t('common.loading')}</p>
{:else if sessions.length === 0 && !error}
  <div class="card">
    {#if search.trim()}
      <EmptyState compact title={t('sessions.no_match')} text={t('sessions.no_match_text')} />
    {:else}
      <EmptyState icon={MessagesSquare} title={t('sessions.empty_title')} text={t('sessions.empty_text')} />
    {/if}
  </div>
{:else if sessions.length > 0}
  <ul class="group-list m-0 list-none p-0">
    {#each sessions as session (session.session_key)}
      {@const info = parse(session.session_key)}
      <li class="flex flex-wrap items-center gap-x-6 gap-y-2.5 py-4">
        <div class="min-w-0 flex-1 basis-[340px]">
          <div class="flex flex-wrap items-center gap-x-2.5 gap-y-1">
            <h2 class="m-0 min-w-0 truncate text-[16px] font-semibold">{info.name}</h2>
            {#if session.persona_id}
              <span class="chip chip-sm chip-muted">
                {t('sessions.persona_saved', { name: personaName(session.persona_id, info.instanceId) })}
              </span>
            {/if}
          </div>
          <p class="m-0 mt-0.5 flex flex-wrap gap-x-4 gap-y-0.5 text-[13.5px] text-fg2">
            {#if info.conversation}
              <span class="max-w-full truncate" title={info.conversation}>
                {t('sessions.conversation', { id: info.conversation })}
              </span>
            {/if}
            <span>
              {session.turn_count === 1
                ? t('sessions.turns_one')
                : t('sessions.turns_n', { n: session.turn_count })}
            </span>
            {#if session.total_tokens_used > 0}
              <span>{t('sessions.tokens_n', { n: session.total_tokens_used.toLocaleString() })}</span>
            {/if}
            <span title={new Date(session.last_active_at * 1000).toLocaleString()}>
              {ago(session.last_active_at)}
            </span>
            {#if info.generation > 0}
              <span>{t('sessions.restarted', { n: info.generation })}</span>
            {/if}
          </p>
          <p class="m-0 mt-0.5 truncate font-mono text-[12px] text-fg3" title={session.session_key}>
            {session.session_key}
          </p>
        </div>
        <div class="ml-auto flex items-center gap-2">
          <Button type="button" size="sm" onclick={() => openPersona(session, info)}>
            {t('sessions.persona_btn')}
          </Button>
          <Button
            type="button"
            variant="text" size="sm"
            disabled={busy[session.session_key] || session.turn_count === 0}
            onclick={() => void reset(session, info.name)}
          >
            {t('sessions.reset')}
          </Button>
        </div>
      </li>
    {/each}
  </ul>
{/if}

<Modal
  open={binding !== null}
  title={t('sessions.persona_title', { name: binding?.name ?? '' })}
  locked={bindSaving}
  onclose={() => (binding = null)}
>
  {#if binding}
    {@const savedPersona = sessions.find((session) => session.session_key === binding?.key)?.persona_id}
    <form
      id="session-persona"
      class="flex flex-col gap-3"
      onsubmit={(e) => {
        e.preventDefault();
        void applyPersona();
      }}
    >
      <div>
        <label class="label" for="session-persona-select">{t('nav.personas')}</label>
        <Select id="session-persona-select" bind:value={binding.persona}>
          <option value="">{t('sessions.persona_none')}</option>
          {#if savedPersona && !personasStore.library.some((persona) => persona.id === savedPersona)}
            <!-- Keep the saved legacy choice visible without offering other generated personas. -->
            <option value={savedPersona}>{personaName(savedPersona, parse(binding.key).instanceId)}</option>
          {/if}
          {#each personasStore.library as persona (persona.id)}
            <option value={persona.id}>{persona.name}</option>
          {/each}
        </Select>
      </div>
      {#if binding.pinned}
        <div class="notice notice-info">{t('sessions.persona_pinned')}</div>
      {:else}
        <p class="m-0 hint">{t('sessions.persona_hint')}</p>
      {/if}
      {#if bindError}
        <div class="notice notice-bad"><span class="min-w-0 break-words">{bindError}</span></div>
      {/if}
    </form>
  {/if}

  {#snippet footer()}
    <Button type="button" disabled={bindSaving} onclick={() => (binding = null)}>
      {t('common.cancel')}
    </Button>
    <Button type="submit" form="session-persona" variant="filled" disabled={bindSaving}>
      {t('sessions.persona_apply')}
    </Button>
  {/snippet}
</Modal>
