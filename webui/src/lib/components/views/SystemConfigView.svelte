<script lang="ts">
import {
  CheckCircle2,
  Copy,
  Database,
  HardDrive,
  Network,
  RefreshCw,
  Server,
} from 'lucide-svelte';
import { contextPolicyStore } from '../../stores/contextPolicy.svelte';
import { i18n, t } from '../../stores/i18n.svelte';
import { nodeStore } from '../../stores/node.svelte';
import { providersStore } from '../../stores/providers.svelte';
import {
  describeReplyPolicy,
  replyPolicyStore,
} from '../../stores/replyPolicy.svelte';
import type { ContextPolicy, ReplyMode, ReplyPolicy } from '../../types';

let copiedSnippet = $state(false);

/** Reply-policy draft, seeded from the node once its policy has been read. */
let policyMode = $state<ReplyMode>('always');
let policyProbability = $state(0.5);
let policyRequested = false;
let policySeeded = false;

/** Modes the node accepts, in the order the editor renders them. */
const replyModeKeys: { value: ReplyMode; labelKey: string }[] = [
  { value: 'always', labelKey: 'reply.mode_always' },
  { value: 'mention', labelKey: 'reply.mode_mention' },
  { value: 'probability', labelKey: 'reply.mode_probability' },
  { value: 'never', labelKey: 'reply.mode_never' },
];

// The policy lives on the node, so it is read once when the view opens; the draft is seeded from
// that answer rather than from a guessed default, otherwise saving would silently overwrite it.
$effect(() => {
  if (!policyRequested) {
    policyRequested = true;
    void replyPolicyStore.load();
  }
  const policy = replyPolicyStore.policy;
  if (policy && !policySeeded) {
    policyMode = policy.mode;
    policyProbability = policy.probability;
    policySeeded = true;
  }
});

let contextDraft = $state<ContextPolicy>({
  include_channel_id: false,
  include_sender_id: false,
  include_timestamp: false,
});
let contextRequested = false;
let contextSeeded = false;

// The context policy is read once and seeded into the draft, mirroring the reply policy above.
$effect(() => {
  if (!contextRequested) {
    contextRequested = true;
    void contextPolicyStore.load();
  }
  const policy = contextPolicyStore.policy;
  if (policy && !contextSeeded) {
    contextDraft = { ...policy };
    contextSeeded = true;
  }
});

/** Flips one context switch and persists the result immediately. */
async function saveContextPolicy(next: ContextPolicy) {
  contextDraft = next;
  await contextPolicyStore.save(next);
}

/** Applies the draft to the running node. */
async function saveReplyPolicy() {
  const policy: ReplyPolicy = {
    mode: policyMode,
    probability: policyProbability,
  };
  await replyPolicyStore.save(policy);
}

function copySocketPath(path: string) {
  navigator.clipboard.writeText(path);
  copiedSnippet = true;
  setTimeout(() => {
    copiedSnippet = false;
  }, 2000);
}
</script>

<div class="p-6 space-y-6 max-w-7xl mx-auto">
  <!-- System Architecture Banner -->
  <div class="bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl p-6 shadow-xs">
    <div class="flex items-center justify-between pb-4 border-b border-zinc-100 dark:border-zinc-800">
      <div class="flex items-center gap-3">
        <div class="p-2.5 rounded-xl bg-zinc-100 dark:bg-zinc-800 text-zinc-800 dark:text-zinc-200">
          <Server class="w-5 h-5" />
        </div>
        <div>
          <h3 class="text-base font-semibold text-zinc-900 dark:text-zinc-100">{t('providers.system_config_title')}</h3>
          <p class="text-xs text-zinc-500 font-mono mt-0.5">
            {i18n.locale === 'zh' ? '微内核节点底层运行参数与持久化路径配置' : 'Microkernel runtime parameters & persistent paths configuration'}
          </p>
        </div>
      </div>
      <div class="flex items-center gap-2">
        {#if copiedSnippet}
          <span class="text-xs font-mono text-emerald-500 flex items-center gap-1.5">
            <CheckCircle2 class="w-4 h-4" />
            Copied!
          </span>
        {/if}
        <button
          onclick={() => providersStore.refresh()}
          class="px-3 py-1.5 bg-zinc-100 dark:bg-zinc-800 hover:bg-zinc-200 dark:hover:bg-zinc-700 text-zinc-800 dark:text-zinc-200 rounded-md text-xs font-mono flex items-center gap-1.5 transition cursor-pointer"
        >
          <RefreshCw class="w-3.5 h-3.5" />
          <span>{t('common.refresh')}</span>
        </button>
      </div>
    </div>

    <!-- System parameters grid -->
    <div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4 mt-5 font-mono text-xs">
      <!-- IPC Socket -->
      <div class="p-4 bg-zinc-50 dark:bg-zinc-950/50 rounded-xl border border-zinc-100 dark:border-zinc-800/80">
        <div class="flex items-center justify-between mb-1.5">
          <span class="text-zinc-400 flex items-center gap-1.5">
            <Network class="w-4 h-4" />
            {t('providers.ipc_socket')}
          </span>
          <button
            onclick={() => copySocketPath(providersStore.systemConfig?.ipc_socket_path ?? './run/core.sock')}
            class="text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 cursor-pointer"
            title="Copy Socket Path"
          >
            <Copy class="w-3.5 h-3.5" />
          </button>
        </div>
        <span class="text-zinc-900 dark:text-zinc-100 font-medium text-sm truncate block" title={providersStore.systemConfig?.ipc_socket_path}>
          {providersStore.systemConfig?.ipc_socket_path ?? './run/core.sock'}
        </span>
      </div>

      <!-- Run Directory -->
      <div class="p-4 bg-zinc-50 dark:bg-zinc-950/50 rounded-xl border border-zinc-100 dark:border-zinc-800/80">
        <span class="text-zinc-400 flex items-center gap-1.5 mb-1.5">
          <HardDrive class="w-4 h-4" />
          {t('providers.run_dir')}
        </span>
        <span class="text-zinc-900 dark:text-zinc-100 font-medium text-sm truncate block">
          {providersStore.systemConfig?.run_dir ?? './run'}
        </span>
      </div>

      <!-- Data Directory -->
      <div class="p-4 bg-zinc-50 dark:bg-zinc-950/50 rounded-xl border border-zinc-100 dark:border-zinc-800/80">
        <span class="text-zinc-400 flex items-center gap-1.5 mb-1.5">
          <Database class="w-4 h-4" />
          {t('providers.data_dir')}
        </span>
        <span class="text-zinc-900 dark:text-zinc-100 font-medium text-sm truncate block">
          {providersStore.systemConfig?.data_dir ?? './data'}
        </span>
      </div>

      <!-- Memory Sliding Window -->
      <div class="p-4 bg-zinc-50 dark:bg-zinc-950/50 rounded-xl border border-zinc-100 dark:border-zinc-800/80">
        <span class="text-zinc-400 block mb-1.5">{t('providers.memory_window')}</span>
        <span class="text-zinc-900 dark:text-zinc-100 font-medium text-sm">
          {providersStore.systemConfig?.memory_window ?? 40} turns (Sliding Window FIFO)
        </span>
      </div>

      <!-- OS and Architecture -->
      <div class="p-4 bg-zinc-50 dark:bg-zinc-950/50 rounded-xl border border-zinc-100 dark:border-zinc-800/80">
        <span class="text-zinc-400 block mb-1.5">{t('providers.os_arch')}</span>
        <span class="text-zinc-900 dark:text-zinc-100 font-medium text-sm">
          {providersStore.systemConfig?.environment.os ?? 'linux'} ({providersStore.systemConfig?.environment.arch ?? 'x86_64'})
        </span>
      </div>
    </div>
  </div>

  <!-- Node-wide reply policy: inherited by every instance without an override. -->
  <div class="bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl p-6 shadow-xs">
    <div class="flex items-start justify-between pb-4 border-b border-zinc-100 dark:border-zinc-800 gap-4">
      <div>
        <h3 class="text-base font-semibold text-zinc-900 dark:text-zinc-100">{t('reply.title')}</h3>
        <p class="text-xs text-zinc-500 mt-0.5">{t('instances.reply_policy_hint')}</p>
      </div>
      <span class="text-xs font-mono text-zinc-400 text-right shrink-0">
        {t('reply.node_current', {
          policy:
            replyPolicyStore.description ||
            describeReplyPolicy(replyPolicyStore.policy),
        })}
      </span>
    </div>

    <div class="mt-5 grid grid-cols-1 md:grid-cols-3 gap-5">
      <div class="md:col-span-2">
        <div class="flex flex-wrap gap-2">
          {#each replyModeKeys as choice (choice.value)}
            <button
              onclick={() => (policyMode = choice.value)}
              class="px-3 py-1.5 rounded-lg text-xs font-medium border transition cursor-pointer
                {policyMode === choice.value
                  ? 'bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-950 border-transparent'
                  : 'bg-zinc-50 dark:bg-zinc-950/50 text-zinc-600 dark:text-zinc-400 border-zinc-200 dark:border-zinc-800 hover:border-zinc-300 dark:hover:border-zinc-700'}"
            >
              {t(choice.labelKey)}
            </button>
          {/each}
        </div>
      </div>

      {#if policyMode === 'probability'}
        <div>
          <span class="text-xs text-zinc-400 block mb-2 font-mono">{t('reply.probability')}</span>
          <input
            type="range"
            min="0"
            max="1"
            step="0.05"
            bind:value={policyProbability}
            class="w-full accent-indigo-600 cursor-pointer"
          />
          <span class="text-xs font-mono text-zinc-500">{Math.round(policyProbability * 100)}%</span>
        </div>
      {/if}
    </div>

    {#if replyPolicyStore.error}
      <p class="text-xs text-rose-500 mt-4 font-mono">{replyPolicyStore.error}</p>
    {/if}
    {#if replyPolicyStore.notice}
      <p class="text-xs text-emerald-500 mt-4 font-mono">{replyPolicyStore.notice}</p>
    {/if}

    <div class="mt-5 flex justify-end">
      <button
        onclick={saveReplyPolicy}
        disabled={replyPolicyStore.saving}
        class="px-4 py-1.5 bg-zinc-900 dark:bg-zinc-100 hover:bg-zinc-700 dark:hover:bg-zinc-200 disabled:opacity-50 text-white dark:text-zinc-950 rounded-md text-xs font-medium transition cursor-pointer"
      >
        {replyPolicyStore.saving ? t('models.saving') : t('common.save')}
      </button>
    </div>
  </div>
  <!-- Context extras: what besides the message itself reaches the model. -->
  <div class="bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl p-6 shadow-xs">
    <div class="pb-4 border-b border-zinc-100 dark:border-zinc-800">
      <h3 class="text-base font-semibold text-zinc-900 dark:text-zinc-100">{t('context.title')}</h3>
      <p class="text-xs text-zinc-500 mt-0.5">{t('context.hint')}</p>
    </div>

    <div class="mt-5 space-y-4">
      <label class="flex items-start justify-between gap-4 cursor-pointer select-none">
        <span>
          <span class="block text-sm font-medium text-zinc-800 dark:text-zinc-200">{t('context.channel_id')}</span>
          <span class="block text-xs text-zinc-500 mt-0.5">{t('context.channel_id_hint')}</span>
        </span>
        <input
          type="checkbox"
          checked={contextDraft.include_channel_id}
          onchange={(e) =>
            saveContextPolicy({
              ...contextDraft,
              include_channel_id: e.currentTarget.checked,
            })}
          class="mt-1 rounded text-indigo-600 focus:ring-0 w-4 h-4 shrink-0"
        />
      </label>

      <label class="flex items-start justify-between gap-4 cursor-pointer select-none">
        <span>
          <span class="block text-sm font-medium text-zinc-800 dark:text-zinc-200">{t('context.sender_id')}</span>
          <span class="block text-xs text-zinc-500 mt-0.5">{t('context.sender_id_hint')}</span>
        </span>
        <input
          type="checkbox"
          checked={contextDraft.include_sender_id}
          onchange={(e) =>
            saveContextPolicy({
              ...contextDraft,
              include_sender_id: e.currentTarget.checked,
            })}
          class="mt-1 rounded text-indigo-600 focus:ring-0 w-4 h-4 shrink-0"
        />
      </label>

      <label class="flex items-start justify-between gap-4 cursor-pointer select-none">
        <span>
          <span class="block text-sm font-medium text-zinc-800 dark:text-zinc-200">{t('context.timestamp')}</span>
          <span class="block text-xs text-zinc-500 mt-0.5">{t('context.timestamp_hint')}</span>
        </span>
        <input
          type="checkbox"
          checked={contextDraft.include_timestamp}
          onchange={(e) =>
            saveContextPolicy({
              ...contextDraft,
              include_timestamp: e.currentTarget.checked,
            })}
          class="mt-1 rounded text-indigo-600 focus:ring-0 w-4 h-4 shrink-0"
        />
      </label>
    </div>

    {#if contextPolicyStore.error}
      <p class="text-xs text-rose-500 mt-4 font-mono">{contextPolicyStore.error}</p>
    {/if}
    {#if contextPolicyStore.notice}
      <p class="text-xs text-emerald-500 mt-4 font-mono">{t('context.updated')}</p>
    {/if}
  </div>
</div>
