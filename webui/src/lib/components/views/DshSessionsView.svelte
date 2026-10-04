<script lang="ts">
import { RefreshCw } from 'lucide-svelte';
import { onMount } from 'svelte';
import { api } from '../../api/client';
import { dshConsoleTarget, dshHistory, type DshMessage } from '../../dsh-history';
import { errorText } from '../../format';
import { confirmDialog } from '../../stores/confirm.svelte';
import { t } from '../../stores/i18n.svelte';
import { chatStore } from '../../stores/chat.svelte';
import { router } from '../../stores/router.svelte';
import { agentsStore } from '../../stores/agents.svelte';
import { instancesStore } from '../../stores/instances.svelte';
import type { DshRecord, DshSession, DshSnapshot } from '../../types';
import DshModelPicker from '../settings/DshModelPicker.svelte';
import Button from '../ui/Button.svelte';
import Modal from '../ui/Modal.svelte';
import PageHead from '../ui/PageHead.svelte';
import TextField from '../ui/TextField.svelte';

let sessions = $state<DshSession[]>([]);
let loading = $state(false);
let loaded = $state(false);
let error = $state<string | null>(null);
let busy = $state(false);
let selected = $state<DshSession | null>(null);
let snapshot = $state<DshSnapshot | null>(null);
let records = $state<DshRecord[]>([]);
let messages = $state<DshMessage[]>([]);
let hasMore = $state(false);
let title = $state('');
let model = $state('');

async function load() {
  loading = true;
  error = null;
  try { sessions = await api.getDshSessions(); }
  catch (e) { error = errorText(e); }
  finally { loading = false; loaded = true; }
}
onMount(() => { void load(); });

/** Keeps paging anchored to this remote snapshot even if the agent appends new events. */
async function open(session: DshSession) {
  selected = session;
  snapshot = null;
  records = [];
  messages = [];
  hasMore = false;
  error = null;
  title = session.projections?.values.title?.title ?? '';
  const next = session.projections?.values.modelSelection?.next;
  model = next ? `${next.provider}/${next.model}` : '';
  busy = true;
  try {
    const result = await api.getDshSession(session.sessionId);
    messages = dshHistory(result.records);
    records = result.records;
    snapshot = result;
    hasMore = result.hasMore;
  } catch (e) { error = errorText(e); }
  finally { busy = false; }
}

async function earlier() {
  if (!selected || !snapshot || !records.length || busy) return;
  busy = true;
  error = null;
  try {
    const before = records[0].event.seq;
    const page = await api.getDshHistory(selected.sessionId, snapshot.cursor, before);
    if (!page.records.length || page.records.at(-1)!.event.seq >= before) {
      throw new Error('DSH history page did not advance');
    }
    const combined = [...page.records, ...records];
    messages = dshHistory(combined);
    records = combined;
    hasMore = page.hasMore;
  } catch (e) { error = errorText(e); }
  finally { busy = false; }
}

async function change(action: () => Promise<unknown>) {
  busy = true;
  error = null;
  try { await action(); await load(); }
  catch (e) { error = errorText(e); }
  finally { busy = false; }
}

async function saveModel() {
  if (!selected || !model) return;
  const separator = model.indexOf('/');
  if (separator <= 0) return;
  const id = selected.sessionId;
  await change(() => api.selectDshModel(id, model.slice(0, separator), model.slice(separator + 1)));
}

async function archive(session: DshSession) {
  const yes = await confirmDialog({
    title: t('dsh.archive'), message: t('dsh.archive_hint'), confirm: t('dsh.archive'), danger: true,
  });
  if (!yes) return;
  await change(async () => {
    await api.archiveDshSession(session.sessionId);
    selected = null;
  });
}

function canResume(session: DshSession): boolean {
  const target = dshConsoleTarget(session.sessionId);
  if (target === null) return false;
  const instance = target ? instancesStore.find(target.slice(2)) : undefined;
  if (target && !instance) return false;
  return (instance?.agent ?? agentsStore.defaultAgent) === 'dsh';
}

async function resume(session: DshSession) {
  busy = true;
  error = null;
  try {
    await chatStore.resumeDsh(session);
    router.navigate('chat', chatStore.target.startsWith('i:') ? chatStore.target.slice(2) : null);
  } catch (e) { error = errorText(e); }
  finally { busy = false; }
}
</script>

<PageHead title={t('nav.sessions')}>
  {#snippet sub()}<span>{t('dsh.sessions_hint')}</span>{/snippet}
  {#snippet actions()}
    <Button disabled={loading || busy} onclick={() => void load()}>
      <RefreshCw size={16} />{t('platforms.refresh')}
    </Button>
  {/snippet}
</PageHead>
{#if error && !selected}<div class="notice notice-bad">{error}</div>{/if}
{#if !loaded}<p class="hint">{t('common.loading')}</p>{/if}
{#if loaded && sessions.length === 0 && !error}<p class="hint">{t('dsh.no_sessions')}</p>{/if}
<ul class="m-0 flex list-none flex-col gap-2 p-0">
  {#each sessions as session (session.sessionId)}
    <li class="card flex flex-wrap items-center gap-3 px-5 py-4">
      <div class="min-w-0 flex-1">
        <p class="m-0 truncate font-medium">{session.projections?.values.title?.title || session.sessionId}</p>
        <p class="m-0 truncate text-[12px] text-fg3">{session.sessionId}</p>
        <p class="m-0 text-[13px] text-fg2">{new Date(session.updatedAt).toLocaleString()}</p>
      </div>
      {#if session.running}<span class="chip">{t('dsh.running')}</span>{/if}
      <Button size="sm" disabled={busy} onclick={() => void open(session)}>{t('dsh.history')}</Button>
      {#if canResume(session)}
        <Button size="sm" disabled={busy || session.running} onclick={() => void resume(session)}>{t('dsh.continue')}</Button>
      {/if}
      <Button size="sm" disabled={busy || !session.running} onclick={() => void change(() => api.stopDshSession(session.sessionId))}>{t('chat.stop')}</Button>
      <Button size="sm" disabled={busy} onclick={() => void archive(session)}>{t('dsh.archive')}</Button>
    </li>
  {/each}
</ul>

<Modal open={selected !== null} title={t('dsh.history')} locked={busy} onclose={() => selected = null}>
  {#if selected}
    {@const sessionId = selected.sessionId}
    <p class="hint break-all">{sessionId}</p>
    <div class="flex flex-col gap-3">
      <label class="label" for="dsh-title">{t('dsh.title')}</label>
      <TextField id="dsh-title" bind:value={title} />
      <Button disabled={busy} onclick={() => void change(() => api.renameDshSession(sessionId, title))}>{t('common.save')}</Button>
      <label class="label" for="dsh-model">{t('chat.model')}</label>
      <DshModelPicker bind:value={model} disabled={busy} />
      <Button disabled={busy || !model} onclick={() => void saveModel()}>{t('common.save')}</Button>
      {#if error}<div class="notice notice-bad">{error}</div>{/if}
      {#if hasMore}<Button disabled={busy} onclick={() => void earlier()}>{t('dsh.earlier')}</Button>{/if}
      {#each messages as message (message.seq)}
        <article class="rounded-xl bg-sunk p-3">
          <p class="m-0 text-[12px] font-medium text-fg2">{message.role}</p>
          {#if message.reasoning}
            <details><summary>{t('chat.thinking')}</summary><p class="whitespace-pre-wrap">{message.reasoning}</p></details>
          {/if}
          <p class="m-0 break-words whitespace-pre-wrap">{message.text}</p>
          {#each message.blocks as block}
            {#if block.type === 'image' && typeof block.data === 'string' && typeof block.mediaType === 'string'}
              <img alt={t('dsh.image')} src={`data:${block.mediaType};base64,${block.data}`} class="max-h-64 max-w-full" />
            {:else if block.type === 'tool-call'}
              <p class="hint">{t('dsh.tool')}: {String(block.name ?? '')}</p>
            {/if}
          {/each}
        </article>
      {/each}
    </div>
  {/if}
</Modal>
