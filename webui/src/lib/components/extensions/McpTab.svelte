<script lang="ts">
import { Plus, RefreshCw, Server, Trash2 } from 'lucide-svelte';
import { untrack } from 'svelte';
import { api } from '../../api/client';
import { errorText } from '../../format';
import { confirmDialog } from '../../stores/confirm.svelte';
import { t } from '../../stores/i18n.svelte';
import { toasts } from '../../stores/toast.svelte';
import type { McpServerView, McpTransport } from '../../types';
import Button from '../ui/Button.svelte';
import EmptyState from '../ui/EmptyState.svelte';
import Modal from '../ui/Modal.svelte';
import Seg from '../ui/Seg.svelte';
import Switch from '../ui/Switch.svelte';
import TextField from '../ui/TextField.svelte';

/**
 * MCP servers: tool providers the node connects to over stdio or HTTP.
 *
 * The node routes their tools like a plugin's, so adding or switching a server here changes what
 * the model can call on its next turn, without a restart.
 */

let servers = $state<McpServerView[]>([]);
let loaded = $state(false);
let loading = $state(false);
let error = $state<string | null>(null);
let busy = $state<Record<string, boolean>>({});

// Editor. `editingId === null` creates a server; an id replaces that definition.
let editorOpen = $state(false);
let editingId = $state<string | null>(null);
let formId = $state('');
let formName = $state('');
let formKind = $state<'stdio' | 'http'>('stdio');
let formCommand = $state('');
let formArgs = $state('');
let formUrl = $state('');
let formPairs = $state('');
let saving = $state(false);
let formError = $state<string | null>(null);

async function load() {
  loading = true;
  error = null;
  try {
    servers = (await api.getMcpServers()).servers;
  } catch (e) {
    error = errorText(e);
  } finally {
    loading = false;
    loaded = true;
  }
}

$effect(() => {
  untrack(() => void load());
});

/** Parses `KEY=VALUE` (or `Key: Value`) lines; a malformed line is an error, never skipped. */
function parsePairs(raw: string): Record<string, string> {
  const parsed: Record<string, string> = {};
  for (const line of raw.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const separator = trimmed.includes('=') ? '=' : ':';
    const index = trimmed.indexOf(separator);
    const key = index > 0 ? trimmed.slice(0, index).trim() : '';
    const value = index > 0 ? trimmed.slice(index + 1).trim() : '';
    if (!key || !value)
      throw new Error(t('extensions.mcp_bad_pair', { line: trimmed }));
    parsed[key] = value;
  }
  return parsed;
}

function formatPairs(values: Record<string, string>): string {
  return Object.entries(values)
    .map(([key, value]) => `${key}=${value}`)
    .join('\n');
}

function openCreate() {
  editingId = null;
  formId = '';
  formName = '';
  formKind = 'stdio';
  formCommand = '';
  formArgs = '';
  formUrl = '';
  formPairs = '';
  formError = null;
  editorOpen = true;
}

function openEdit(server: McpServerView) {
  editingId = server.id;
  formId = server.id;
  formName = server.name;
  formKind = server.transport.type;
  formCommand = '';
  formArgs = '';
  formUrl = '';
  if (server.transport.type === 'stdio') {
    formCommand = server.transport.command;
    formArgs = server.transport.args.join(' ');
    formPairs = formatPairs(server.transport.env);
  } else {
    formUrl = server.transport.url;
    formPairs = formatPairs(server.transport.headers);
  }
  formError = null;
  editorOpen = true;
}

async function save() {
  formError = null;
  let id: string;
  let transport: McpTransport;
  try {
    id = formId.trim();
    if (!id) throw new Error(t('extensions.mcp_need_id'));
    if (formKind === 'stdio') {
      const command = formCommand.trim();
      if (!command) throw new Error(t('extensions.mcp_need_command'));
      transport = {
        type: 'stdio',
        command,
        args: formArgs.trim() ? formArgs.trim().split(/\s+/) : [],
        env: parsePairs(formPairs),
      };
    } else {
      const url = formUrl.trim();
      if (!url) throw new Error(t('extensions.mcp_need_url'));
      transport = { type: 'http', url, headers: parsePairs(formPairs) };
    }
  } catch (e) {
    formError = errorText(e);
    return;
  }

  saving = true;
  try {
    const saved = await api.upsertMcpServer(id, {
      name: formName.trim() || null,
      transport,
    });
    toasts.ok(
      t('extensions.mcp_saved', { name: saved.name, n: saved.health.tools }),
    );
    editorOpen = false;
    await load();
  } catch (e) {
    formError = errorText(e);
  } finally {
    saving = false;
  }
}

async function setEnabled(server: McpServerView, next: boolean) {
  busy = { ...busy, [server.id]: true };
  try {
    await api.setMcpServerEnabled(server.id, next);
    toasts.ok(
      t(next ? 'extensions.on_toast' : 'extensions.off_toast', {
        name: server.name,
      }),
    );
    await load();
  } catch (e) {
    toasts.error(
      t('extensions.toggle_failed', { name: server.name, error: errorText(e) }),
    );
  } finally {
    busy = { ...busy, [server.id]: false };
  }
}

async function remove(server: McpServerView) {
  const yes = await confirmDialog({
    title: t('extensions.mcp_remove_title', { name: server.name }),
    message: t('extensions.mcp_remove_text'),
    confirm: t('extensions.remove'),
    danger: true,
  });
  if (!yes) return;
  busy = { ...busy, [server.id]: true };
  try {
    await api.removeMcpServer(server.id);
    toasts.ok(t('extensions.removed_toast', { name: server.name }));
    await load();
  } catch (e) {
    toasts.error(errorText(e));
  } finally {
    busy = { ...busy, [server.id]: false };
  }
}

type Tone = 'ok' | 'warn' | 'bad' | 'idle';

function health(server: McpServerView): { tone: Tone; label: string } {
  if (!server.enabled)
    return { tone: 'idle', label: t('extensions.state_off') };
  switch (server.health.state) {
    case 'connected':
      return { tone: 'ok', label: t('extensions.mcp_connected') };
    case 'failed':
      return { tone: 'bad', label: t('extensions.mcp_failed') };
    case 'reconnecting':
      return { tone: 'warn', label: t('extensions.mcp_reconnecting') };
    default:
      return { tone: 'warn', label: t('extensions.mcp_connecting') };
  }
}

const CHIP: Record<Tone, string> = {
  ok: 'chip-ok',
  warn: 'chip-warn',
  bad: 'chip-bad',
  idle: 'chip-muted',
};
</script>

<div class="flex flex-wrap items-center justify-between gap-3 px-1">
  <p class="m-0 max-w-[68ch] hint">{t('extensions.mcp_hint')}</p>
  <div class="flex flex-wrap gap-2.5">
    <Button type="button" disabled={loading} onclick={() => void load()}>
      <RefreshCw size={16} strokeWidth={2} class={loading ? 'animate-spin' : ''} />
      {t('platforms.refresh')}
    </Button>
    <Button type="button" variant="filled" onclick={openCreate}>
      <Plus size={16} strokeWidth={2.2} />
      {t('extensions.mcp_add')}
    </Button>
  </div>
</div>

{#if error}
  <div class="notice notice-bad">{error}</div>
{/if}

{#if loaded && servers.length === 0 && !error}
  <div class="card">
    <EmptyState icon={Server} title={t('extensions.mcp_empty')} text={t('extensions.mcp_empty_text')}>
      {#snippet action()}
        <Button type="button" variant="filled" onclick={openCreate}>
          <Plus size={16} strokeWidth={2.2} />
          {t('extensions.mcp_add')}
        </Button>
      {/snippet}
    </EmptyState>
  </div>
{:else if servers.length > 0}
  <div class="group-list">
    {#each servers as server (server.id)}
      {@const status = health(server)}
      <article class="flex flex-wrap items-start gap-x-6 gap-y-3 py-5">
        <div class="flex min-w-0 flex-1 basis-[340px] flex-col gap-1.5">
          <div class="flex flex-wrap items-center gap-x-2.5 gap-y-1">
            <h2 class="m-0 text-[17px] font-semibold">{server.name}</h2>
            <span class="chip chip-sm {CHIP[status.tone]}">
              {#if status.tone !== 'idle'}<i class="dot dot-{status.tone}"></i>{/if}
              {status.label}
            </span>
            {#if server.enabled}
              <span class="text-[13.5px] text-fg2">
                {server.health.tools === 1
                  ? t('extensions.mcp_tools_one')
                  : t('extensions.mcp_tools', { n: server.health.tools })}
              </span>
            {/if}
          </div>
          <code class="block max-w-full truncate text-[12.5px] text-fg2" title={server.transport.type === 'stdio'
            ? `${server.transport.command} ${server.transport.args.join(' ')}`
            : server.transport.url}>
            {server.transport.type === 'stdio'
              ? `${server.transport.command} ${server.transport.args.join(' ')}`
              : server.transport.url}
          </code>
          <p class="m-0 flex flex-wrap gap-x-4 text-[12.5px] text-fg3">
            <span>{server.id}</span>
            <span>{server.transport.type === 'stdio' ? 'stdio' : 'HTTP'}</span>
            {#if server.enabled && server.health.failures > 0}
              <span class="text-warn">{t('extensions.mcp_failures', { n: server.health.failures })}</span>
            {/if}
          </p>
          {#if server.enabled && server.health.last_error}
            <div class="notice notice-bad mt-1">
              <span class="min-w-0 break-words">{server.health.last_error}</span>
            </div>
          {/if}
        </div>

        <div class="ml-auto flex items-center gap-2.5">
          <Button
            type="button"
            variant="text" size="sm" square
            aria-label={t('extensions.mcp_remove_title', { name: server.name })}
            title={t('extensions.remove')}
            disabled={busy[server.id]}
            onclick={() => void remove(server)}
          >
            <Trash2 size={16} strokeWidth={2} />
          </Button>
          <Button type="button" size="sm" onclick={() => openEdit(server)}>
            {t('extensions.edit')}
          </Button>
          <Switch
            checked={server.enabled}
            disabled={busy[server.id]}
            label={server.enabled
              ? t('platforms.turn_off', { name: server.name })
              : t('platforms.turn_on', { name: server.name })}
            onchange={(next) => void setEnabled(server, next)}
          />
        </div>
      </article>
    {/each}
  </div>
{:else if !error}
  <p class="m-0 px-1 hint">{t('common.loading')}</p>
{/if}

<Modal
  open={editorOpen}
  title={editingId ? t('extensions.mcp_edit_title', { name: formName || editingId }) : t('extensions.mcp_add')}
  locked={saving}
  onclose={() => (editorOpen = false)}
>
  <form
    id="mcp-editor"
    class="flex flex-col gap-4"
    onsubmit={(e) => {
      e.preventDefault();
      void save();
    }}
  >
    <div class="grid gap-4 sm:grid-cols-2">
      <div>
        <label class="label" for="mcp-name">{t('extensions.mcp_name')}</label>
        <TextField id="mcp-name" placeholder="Filesystem" bind:value={formName} />
      </div>
      <div>
        <label class="label" for="mcp-id">{t('extensions.mcp_id')}</label>
        <TextField
          id="mcp-id"
          mono
          spellcheck="false"
          placeholder="filesystem"
          disabled={editingId !== null}
          bind:value={formId}
        />
      </div>
    </div>
    <div>
      <span class="label">{t('extensions.mcp_transport')}</span>
      <Seg
        label={t('extensions.mcp_transport')}
        value={formKind}
        onchange={(next: 'stdio' | 'http') => (formKind = next)}
        options={[
          { value: 'stdio', label: t('extensions.mcp_stdio') },
          { value: 'http', label: t('extensions.mcp_http') },
        ]}
      />
    </div>
    {#if formKind === 'stdio'}
      <div>
        <label class="label" for="mcp-command">{t('extensions.mcp_command')}</label>
        <TextField id="mcp-command" mono spellcheck="false" placeholder="npx" bind:value={formCommand} />
      </div>
      <div>
        <label class="label" for="mcp-args">{t('extensions.mcp_args')}</label>
        <TextField
          id="mcp-args"
          mono
          spellcheck="false"
          placeholder="-y @modelcontextprotocol/server-filesystem ./files"
          bind:value={formArgs}
        />
        <p class="m-0 mt-2 hint">{t('extensions.mcp_args_hint')}</p>
      </div>
    {:else}
      <div>
        <label class="label" for="mcp-url">{t('extensions.mcp_url')}</label>
        <TextField
          id="mcp-url"
          mono
          spellcheck="false"
          placeholder="https://example.com/mcp"
          bind:value={formUrl}
        />
      </div>
    {/if}
    <div>
      <label class="label" for="mcp-pairs">
        {formKind === 'stdio' ? t('extensions.mcp_env') : t('extensions.mcp_headers')}
      </label>
      <textarea
        id="mcp-pairs"
        class="input mono"
        rows="3"
        spellcheck="false"
        placeholder="API_KEY=…"
        bind:value={formPairs}
      ></textarea>
      <p class="m-0 mt-2 hint">{t('extensions.mcp_pairs_hint')}</p>
    </div>
    {#if formError}
      <div class="notice notice-bad"><span class="min-w-0 break-words">{formError}</span></div>
    {/if}
  </form>

  {#snippet footer()}
    <Button type="button" disabled={saving} onclick={() => (editorOpen = false)}>
      {t('common.cancel')}
    </Button>
    <Button type="submit" form="mcp-editor" variant="filled" disabled={saving}>
      {saving ? t('platforms.saving') : editingId ? t('extensions.mcp_save') : t('extensions.mcp_add')}
    </Button>
  {/snippet}
</Modal>
