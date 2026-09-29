<script lang="ts">
import {
  Bot,
  BrainCircuit,
  CheckCircle2,
  Send,
  Sliders,
  Sparkles,
  Square,
  Trash2,
  User,
  Wrench,
  XCircle,
} from 'lucide-svelte';
import { api } from '../../api/client';
import { streamChatCompletion } from '../../api/sse';
import { t } from '../../stores/i18n.svelte';
import { modelsStore } from '../../stores/models.svelte';
import { providersStore } from '../../stores/providers.svelte';
import type { ExecutedTool } from '../../types';

interface ChatMessage {
  id: string;
  role: 'user' | 'assistant';
  content: string;
  reasoning?: string;
  executedTools?: ExecutedTool[];
  timestamp: Date;
}

let sessionId = $state('webui:chat');
let inputMessage = $state('');
let isStreaming = $state(false);
let enableTools = $state(true);
let messages = $state<ChatMessage[]>([]);
let abortController: AbortController | null = null;
let chatContainer = $state<HTMLDivElement | null>(null);

// Auto-scroll chat
$effect(() => {
  if (messages.length > 0 && chatContainer) {
    chatContainer.scrollTop = chatContainer.scrollHeight;
  }
});

async function sendMessage() {
  const text = inputMessage.trim();
  if (!text || isStreaming) return;

  const activeModelKey = providersStore.activeModel;
  // The sandbox sends the canonical `<provider>/<model-id>` reference and lets the node resolve it
  // against its provider directory. The browser never receives a credential, so it cannot build a
  // provider of its own — and it no longer needs to.
  const targetModel: string | undefined = activeModelKey || undefined;

  inputMessage = '';
  const userMsg: ChatMessage = {
    id: `usr_${Date.now()}`,
    role: 'user',
    content: text,
    timestamp: new Date(),
  };
  messages = [...messages, userMsg];

  const assistantId = `ast_${Date.now()}`;
  const assistantMsg: ChatMessage = {
    id: assistantId,
    role: 'assistant',
    content: '',
    reasoning: '',
    executedTools: [],
    timestamp: new Date(),
  };
  messages = [...messages, assistantMsg];

  // Neither a chosen model nor a global default model: the node has nothing to answer with.
  if (!targetModel && !modelsStore.defaultModel) {
    messages = messages.map((m) =>
      m.id === assistantId
        ? {
            ...m,
            content:
              '尚未设置全局默认模型。请先在「模型提供商」页面添加提供商，并选定默认模型。',
          }
        : m,
    );
    return;
  }

  isStreaming = true;
  abortController = new AbortController();

  try {
    await streamChatCompletion(
      {
        session_id: sessionId,
        message: text,
        model: targetModel,
        tools: enableTools,
      },
      {
        onChunk: (delta, reasoning) => {
          messages = messages.map((m) => {
            if (m.id !== assistantId) return m;
            return {
              ...m,
              content: m.content + (delta || ''),
              reasoning: reasoning
                ? (m.reasoning || '') + reasoning
                : m.reasoning,
            };
          });
        },
        onFinish: () => {
          isStreaming = false;
        },
        onError: (err) => {
          messages = messages.map((m) => {
            if (m.id !== assistantId) return m;
            return {
              ...m,
              content: m.content
                ? `${m.content}\n[错误: ${err.message}]`
                : `[错误: ${err.message}]`,
            };
          });
          isStreaming = false;
        },
      },
      abortController.signal,
    );
  } catch (err) {
    messages = messages.map((m) => {
      if (m.id !== assistantId) return m;
      return {
        ...m,
        content: m.content
          ? `${m.content}\n[发送请求失败: ${err}]`
          : `[发送请求失败: ${err}]`,
      };
    });
    isStreaming = false;
  }
}

function parseMessageContent(msg: ChatMessage) {
  let reasoning = msg.reasoning || '';
  let content = msg.content || '';

  if (content.includes('<think>')) {
    const startIdx = content.indexOf('<think>');
    const endIdx = content.indexOf('</think>');
    if (endIdx !== -1) {
      const thinkText = content.slice(startIdx + 7, endIdx).trim();
      if (!reasoning) reasoning = thinkText;
      content = (content.slice(0, startIdx) + content.slice(endIdx + 8)).trim();
    } else {
      const thinkText = content.slice(startIdx + 7).trim();
      if (!reasoning) reasoning = thinkText;
      content = content.slice(0, startIdx).trim();
    }
  }

  return { reasoning, content };
}

function handleStop() {
  if (abortController) {
    abortController.abort();
    abortController = null;
  }
  isStreaming = false;
}

function clearChat() {
  messages = [];
}
</script>

<div class="h-full flex flex-col bg-zinc-50/50 dark:bg-zinc-950/50 overflow-hidden">
  <!-- Top Control Bar -->
  <div class="p-3 border-b border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 flex items-center justify-between shrink-0">
    <div class="flex items-center gap-4">
      <div class="flex items-center gap-2">
        <Bot class="w-4.5 h-4.5 text-indigo-500" />
        <span class="text-sm font-semibold text-zinc-900 dark:text-zinc-100">{t('nav.chat')}</span>
      </div>
      <div class="flex items-center gap-1.5 text-xs">
        <label for="chat-session-input" class="text-zinc-400 font-mono text-xs">{t('sessions.session_id')}:</label>
        <input
          id="chat-session-input"
          type="text"
          bind:value={sessionId}
          class="px-2.5 py-1 rounded-md font-mono text-xs sm:text-sm bg-zinc-100 dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 text-zinc-800 dark:text-zinc-200 w-36"
        />
      </div>

      <div class="flex items-center gap-1.5 text-xs">
        <label for="chat-model-select" class="text-zinc-400 font-mono text-xs">模型:</label>
        <select
          id="chat-model-select"
          value={providersStore.activeModel}
          onchange={(e) => providersStore.setActiveModel(e.currentTarget.value)}
          class="px-2.5 py-1 rounded-md font-mono text-xs sm:text-sm bg-zinc-100 dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 text-zinc-800 dark:text-zinc-200 max-w-56 truncate cursor-pointer"
        >
          <option value="">
            {modelsStore.defaultModel
              ? `默认 · ${modelsStore.defaultModel}`
              : '未设置默认模型'}
          </option>
          {#each providersStore.allModelKeys as key}
            <option value={key}>{key}</option>
          {/each}
        </select>
      </div>
    </div>

    <div class="flex items-center gap-3">
      <!-- Toggle tools -->
      <label class="flex items-center gap-1.5 text-xs sm:text-sm text-zinc-600 dark:text-zinc-400 cursor-pointer select-none">
        <input
          type="checkbox"
          bind:checked={enableTools}
          class="rounded text-indigo-600 focus:ring-0 w-3.5 h-3.5"
        />
        <span>{t('playground.enable_tools')}</span>
      </label>

      <!-- Clear messages -->
      <button
        onclick={clearChat}
        class="p-1.5 text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200 transition cursor-pointer"
        title={t('common.clear')}
      >
        <Trash2 class="w-4 h-4" />
      </button>
    </div>
  </div>

  <!-- Messages Scroll Area -->
  <div
    bind:this={chatContainer}
    class="flex-1 p-6 overflow-y-auto space-y-5 max-w-4xl w-full mx-auto"
  >
    {#if messages.length === 0}
      <div class="p-16 text-center space-y-3">
        <div class="w-14 h-14 rounded-2xl bg-indigo-50 dark:bg-indigo-950/60 border border-indigo-200 dark:border-indigo-800/60 flex items-center justify-center mx-auto text-indigo-600 dark:text-indigo-400">
          <Bot class="w-7 h-7" />
        </div>
        <h4 class="text-base font-semibold text-zinc-900 dark:text-zinc-100">{t('title.playground')}</h4>
        <p class="text-sm text-zinc-500 max-w-sm mx-auto leading-relaxed">
          {t('playground.empty_chat')}
        </p>
      </div>
    {:else}
      {#each messages as msg (msg.id)}
        {@const parsed = parseMessageContent(msg)}
        {@const isLastAssistant = msg.role === 'assistant' && msg.id === messages[messages.length - 1]?.id}
        {@const isThinkingNow = isLastAssistant && isStreaming && !parsed.content}

        <div class="flex items-start gap-3.5 text-sm sm:text-base leading-relaxed {msg.role === 'user' ? 'justify-end' : 'justify-start'}">
          {#if msg.role === 'assistant'}
            <div class="w-8 h-8 rounded-xl bg-indigo-600 text-white flex items-center justify-center shrink-0 mt-0.5 shadow-xs">
              <Bot class="w-4.5 h-4.5" />
            </div>
          {/if}

          <div class="space-y-2.5 max-w-[88%] sm:max-w-2xl w-full {msg.role === 'user' ? 'flex flex-col items-end' : ''}">
            <!-- Thought / Reasoning block -->
            {#if parsed.reasoning}
              <details class="group rounded-xl border border-indigo-200/70 dark:border-indigo-900/70 bg-indigo-50/50 dark:bg-indigo-950/25 text-xs sm:text-sm overflow-hidden" open={isThinkingNow}>
                <summary class="flex items-center gap-2 px-3.5 py-2 cursor-pointer text-indigo-700 dark:text-indigo-300 font-mono text-xs sm:text-sm select-none hover:bg-indigo-100/50 dark:hover:bg-indigo-900/40 transition">
                  <BrainCircuit class="w-4 h-4 shrink-0 text-indigo-500 {isThinkingNow ? 'animate-pulse' : ''}" />
                  <span class="font-medium">
                    {isThinkingNow ? '思考中...' : '已深度思考'}
                  </span>
                  <span class="text-xs text-zinc-400">({parsed.reasoning.length} 字)</span>
                </summary>
                <div class="px-3.5 py-2.5 text-xs sm:text-[13px] text-zinc-600 dark:text-zinc-400 font-mono whitespace-pre-wrap border-t border-indigo-100 dark:border-indigo-950 bg-white/60 dark:bg-zinc-950/50 max-h-64 overflow-y-auto leading-relaxed">
                  {parsed.reasoning}
                </div>
              </details>
            {:else if isThinkingNow}
              <div class="flex items-center gap-2 px-3.5 py-2 rounded-xl border border-indigo-200/70 dark:border-indigo-900/70 bg-indigo-50/50 dark:bg-indigo-950/25 text-indigo-600 dark:text-indigo-400 font-mono text-xs sm:text-sm animate-pulse">
                <BrainCircuit class="w-4 h-4 shrink-0" />
                <span>思考中 (Thinking)...</span>
              </div>
            {/if}

            <!-- Main Message Bubble -->
            {#if msg.role === 'user'}
              <div class="p-3.5 sm:p-4 rounded-2xl shadow-2xs text-sm sm:text-base leading-relaxed bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900 font-medium">
                <div class="whitespace-pre-wrap">{msg.content}</div>
              </div>
            {:else if parsed.content}
              <div class="p-3.5 sm:p-4 rounded-2xl shadow-2xs text-sm sm:text-base leading-relaxed bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 text-zinc-800 dark:text-zinc-200">
                <div class="whitespace-pre-wrap">{parsed.content}</div>
              </div>
            {:else if isLastAssistant && isStreaming && parsed.reasoning}
              <div class="flex items-center gap-2 text-zinc-400 font-mono text-xs sm:text-sm px-2 py-1">
                <span class="w-2 h-2 rounded-full bg-indigo-500 animate-pulse"></span>
                <span>正在组织回复...</span>
              </div>
            {:else if !isStreaming && !parsed.reasoning}
              <div class="p-3.5 sm:p-4 rounded-2xl shadow-2xs text-xs sm:text-sm bg-amber-50 dark:bg-amber-950/30 border border-amber-200 dark:border-amber-800/50 text-amber-700 dark:text-amber-300">
                <div class="whitespace-pre-wrap">{parsed.content || '[无文本回复内容]'}</div>
              </div>
            {/if}

            <!-- Executed Tools inspection cards -->
            {#if msg.executedTools && msg.executedTools.length > 0}
              <div class="space-y-1.5">
                {#each msg.executedTools as tool}
                  <div class="p-2.5 rounded-lg bg-zinc-100 dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 flex items-center justify-between text-xs font-mono">
                    <div class="flex items-center gap-2">
                      <Wrench class="w-3.5 h-3.5 text-indigo-500" />
                      <span class="font-bold">{tool.tool_name}</span>
                      <span class="text-zinc-400">({tool.plugin_id})</span>
                    </div>
                    <span class="flex items-center gap-1 {tool.success ? 'text-emerald-500' : 'text-rose-500'} font-semibold">
                      {#if tool.success}
                        <CheckCircle2 class="w-3.5 h-3.5" />
                        <span>Success</span>
                      {:else}
                        <XCircle class="w-3.5 h-3.5" />
                        <span>Failed</span>
                      {/if}
                    </span>
                  </div>
                {/each}
              </div>
            {/if}
          </div>

          {#if msg.role === 'user'}
            <div class="w-8 h-8 rounded-xl bg-zinc-200 dark:bg-zinc-800 text-zinc-600 dark:text-zinc-300 flex items-center justify-center shrink-0 mt-0.5">
              <User class="w-4.5 h-4.5" />
            </div>
          {/if}
        </div>
      {/each}
    {/if}
  </div>

  <!-- Input Bar -->
  <div class="p-4 border-t border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 shrink-0">
    <div class="max-w-4xl mx-auto flex items-center gap-2.5">
      <input
        type="text"
        bind:value={inputMessage}
        onkeydown={(e) => e.key === 'Enter' && sendMessage()}
        placeholder={t('playground.placeholder')}
        class="flex-1 px-4 py-3 rounded-xl bg-zinc-100 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 text-sm sm:text-base text-zinc-900 dark:text-zinc-100 placeholder-zinc-400 focus:outline-hidden focus:border-zinc-400 dark:focus:border-zinc-600"
      />
      {#if isStreaming}
        <button
          onclick={handleStop}
          class="p-3 bg-rose-600 hover:bg-rose-500 text-white rounded-xl font-medium transition cursor-pointer flex items-center justify-center"
          title="Stop generation"
        >
          <Square class="w-4.5 h-4.5 fill-current" />
        </button>
      {:else}
        <button
          onclick={sendMessage}
          disabled={!inputMessage.trim()}
          class="p-3 bg-zinc-900 hover:bg-zinc-800 dark:bg-zinc-100 dark:hover:bg-zinc-200 text-white dark:text-zinc-900 rounded-xl font-medium transition cursor-pointer flex items-center justify-center disabled:opacity-40"
          title={t('playground.send')}
        >
          <Send class="w-4 h-4" />
        </button>
      {/if}
    </div>
  </div>
</div>
