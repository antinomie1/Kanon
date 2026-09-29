<script lang="ts">
import { RefreshCw, Trash2 } from 'lucide-svelte';
import { api } from '../../api/client';
import { t } from '../../stores/i18n.svelte';
import { personasStore } from '../../stores/personas.svelte';
import type { SessionSummary } from '../../types';

let sessions = $state<SessionSummary[]>([]);
let loading = $state(true);
let error = $state<string | null>(null);

// Selected session for persona binding modal
let bindingSession = $state<SessionSummary | null>(null);
let selectedPersona = $state<string>('');
let bindingStatus = $state<string | null>(null);

async function loadData() {
  loading = true;
  error = null;
  try {
    const [sessRes] = await Promise.all([
      api.getSessions(),
      personasStore.load(),
    ]);
    const rawSessions = sessRes.items ?? sessRes.sessions ?? [];
    sessions = rawSessions.map((s) => ({
      ...s,
      session_id: s.session_id ?? s.session_key ?? '',
      active_persona: s.active_persona ?? s.persona_id ?? undefined,
    }));
  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
  } finally {
    loading = false;
  }
}

async function handleResetSession(sessionId: string) {
  if (!confirm(t('sessions.reset_confirm', { id: sessionId }))) return;
  try {
    await api.resetSession(sessionId);
    await loadData();
  } catch (e) {
    alert(
      `${t('sessions.reset_failed')}: ${e instanceof Error ? e.message : String(e)}`,
    );
  }
}

async function applyPersonaSwitch() {
  if (!bindingSession) return;
  bindingStatus = t('sessions.binding');
  try {
    // An empty choice removes the binding: the session then uses the base assistant.
    await api.setSessionPersona(
      bindingSession.session_id,
      selectedPersona || null,
    );
    bindingStatus = null;
    bindingSession = null;
    await loadData();
  } catch (e) {
    bindingStatus = `${t('sessions.bind_failed')}: ${e instanceof Error ? e.message : String(e)}`;
  }
}

$effect(() => {
  loadData();
});
</script>

<div class="p-6 space-y-6 max-w-7xl mx-auto">
  <!-- Top bar -->
  <div class="flex items-center justify-between">
    <div>
      <h3 class="text-base sm:text-lg font-semibold text-zinc-900 dark:text-zinc-100 tracking-tight">{t('title.sessions')}</h3>
      <p class="text-xs sm:text-sm text-zinc-500">{t('subtitle.sessions')}</p>
    </div>
    <button
      onclick={loadData}
      class="px-3 py-1.5 text-xs sm:text-sm font-medium rounded-lg bg-white dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700 transition cursor-pointer flex items-center gap-1.5 shadow-2xs"
    >
      <RefreshCw class="w-4 h-4" />
      <span>{t('common.refresh')}</span>
    </button>
  </div>

  {#if loading}
    <div class="p-12 text-center text-sm text-zinc-400">{t('common.loading')}</div>
  {:else if error}
    <div class="p-4 rounded-lg bg-rose-500/10 border border-rose-500/20 text-rose-600 dark:text-rose-400 text-sm">
      {error}
    </div>
  {:else}
    <div class="space-y-3">
        <h4 class="text-xs sm:text-sm font-semibold text-zinc-700 dark:text-zinc-300 uppercase tracking-wider font-mono">
          {t('sessions.active_sessions')} ({sessions?.length ?? 0})
        </h4>

        {#if !sessions || sessions.length === 0}
          <div class="p-8 rounded-xl border border-dashed border-zinc-300 dark:border-zinc-800 text-center text-zinc-400 text-sm">
            {t('sessions.no_sessions')}
          </div>
        {:else}
          <div class="space-y-2.5">
            {#each sessions as session}
              <div class="p-4 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 shadow-2xs flex items-center justify-between gap-4">
                <div class="space-y-1.5 min-w-0">
                  <div class="flex items-center gap-2">
                    <span class="font-mono font-bold text-sm sm:text-base text-zinc-900 dark:text-zinc-100 truncate">
                      {session.session_id}
                    </span>
                    {#if session.active_persona}
                      <span class="px-2 py-0.5 rounded text-xs font-mono bg-violet-500/10 text-violet-600 dark:text-violet-400 border border-violet-500/20">
                        {session.active_persona}
                      </span>
                    {/if}
                  </div>
                  <div class="flex items-center gap-3.5 text-xs sm:text-sm text-zinc-500 font-mono">
                    <span>{t('sessions.turns')}: <b class="text-zinc-800 dark:text-zinc-200 font-semibold">{session.turn_count}</b></span>
                    <span>{t('sessions.tokens')}: <b class="text-zinc-800 dark:text-zinc-200 font-semibold">{session.total_tokens_used}</b></span>
                  </div>
                </div>

                <div class="flex items-center gap-2 shrink-0">
                  <button
                    onclick={() => {
                      bindingSession = session;
                      selectedPersona = session.active_persona ?? '';
                    }}
                    class="px-3 py-1.5 text-xs sm:text-sm text-zinc-600 dark:text-zinc-400 hover:text-zinc-900 dark:hover:text-zinc-100 border border-zinc-200 dark:border-zinc-700 rounded-md hover:bg-zinc-50 dark:hover:bg-zinc-800 transition cursor-pointer"
                  >
                    {t('sessions.persona')}
                  </button>
                  <button
                    onclick={() => handleResetSession(session.session_id)}
                    class="p-1.5 text-zinc-400 hover:text-rose-500 transition cursor-pointer"
                    title={t('sessions.reset')}
                  >
                    <Trash2 class="w-4 h-4" />
                  </button>
                </div>
              </div>
            {/each}
          </div>
        {/if}
    </div>
  {/if}
</div>

<!-- Persona Switch Modal -->
{#if bindingSession}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="fixed inset-0 bg-black/40 backdrop-blur-xs z-50 flex items-center justify-center p-4"
    onclick={() => (bindingSession = null)}
    role="button"
    tabindex="-1"
  >
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div
      class="w-full max-w-md bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl shadow-2xl p-6 space-y-4"
      onclick={(e) => e.stopPropagation()}
      role="dialog"
      tabindex="-1"
    >
      <div class="flex items-center justify-between border-b border-zinc-200 dark:border-zinc-800 pb-3">
        <h3 class="text-base font-semibold text-zinc-900 dark:text-zinc-100">
          {t('sessions.bind_title')}
        </h3>
        <button
          onclick={() => (bindingSession = null)}
          class="text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200 text-xs sm:text-sm font-mono cursor-pointer"
        >
          {t('common.cancel')}
        </button>
      </div>

      <div class="space-y-2">
        <label for="session-persona-select" class="block text-xs sm:text-sm font-medium text-zinc-600 dark:text-zinc-400">{t('sessions.bind_select')}</label>
        <select
          id="session-persona-select"
          bind:value={selectedPersona}
          class="w-full p-2.5 bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-sm font-mono text-zinc-900 dark:text-zinc-100"
        >
          <option value="">{t('sessions.bind_none')}</option>
          {#each personasStore.library as p (p.id)}
            <option value={p.id}>{p.name}</option>
          {/each}
        </select>
        <p class="text-[11px] text-zinc-400">{t('sessions.bind_hint')}</p>
      </div>

      {#if bindingStatus}
        <div class="text-xs sm:text-sm text-rose-500 font-mono">{bindingStatus}</div>
      {/if}

      <div class="flex justify-end gap-2 pt-2">
        <button
          onclick={() => (bindingSession = null)}
          class="px-3.5 py-2 text-xs sm:text-sm text-zinc-600 dark:text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded-lg transition cursor-pointer"
        >
          {t('common.cancel')}
        </button>
        <button
          onclick={applyPersonaSwitch}
          class="px-4 py-2 text-xs sm:text-sm bg-indigo-600 hover:bg-indigo-500 text-white rounded-lg font-medium transition cursor-pointer"
        >
          {t('sessions.bind_apply')}
        </button>
      </div>
    </div>
  </div>
{/if}
