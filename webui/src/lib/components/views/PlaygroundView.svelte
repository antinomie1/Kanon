<script lang="ts">
import { ArrowUp, MessageSquarePlus, Square } from 'lucide-svelte';
import { tick, untrack } from 'svelte';
import { type ChatTurn, chatStore as chat } from '../../stores/chat.svelte';
import { t } from '../../stores/i18n.svelte';
import { instancesStore } from '../../stores/instances.svelte';
import { modelsStore } from '../../stores/models.svelte';
import { personasStore } from '../../stores/personas.svelte';
import { router } from '../../stores/router.svelte';
import { toasts } from '../../stores/toast.svelte';
import Button from '../ui/Button.svelte';
import PageHead from '../ui/PageHead.svelte';
import Select from '../ui/Select.svelte';
import Switch from '../ui/Switch.svelte';

/**
 * Test chat: talk to the model straight from the console, as the base assistant, as one of the
 * instances, or with a persona, to check that a model, a prompt and the tools behave.
 *
 * `#/chat/<instance-id>` opens it set up as that instance, which is where an instance's
 * "Test chat" button lands.
 */

let draft = $state('');
let scroller = $state<HTMLDivElement>();
let composer = $state<HTMLTextAreaElement>();
/** Whether the view follows new text; off once the reader scrolls up to read something. */
let stick = true;

const instance = $derived(
  chat.target.startsWith('i:')
    ? instancesStore.find(chat.target.slice(2))
    : undefined,
);
const persona = $derived(
  chat.target.startsWith('p:')
    ? personasStore.all.find((p) => p.id === chat.target.slice(2))
    : undefined,
);

/** Name shown over the replies, so it is clear who is answering. */
const speaker = $derived(
  instance?.name ?? persona?.name ?? t('chat.assistant'),
);

/** Model used when the picker is left on its first entry. */
const inheritedModel = $derived(instance?.model ?? modelsStore.defaultModel);
const model = $derived(chat.model || inheritedModel || undefined);

/**
 * Persona sent with every message. An instance's own prompt is published by the node as the
 * persona `instance:<id>`, which wins over the persona it picked, just as in real conversations.
 */
const personaId = $derived.by(() => {
  if (instance) {
    return instance.system_prompt?.trim()
      ? `instance:${instance.id}`
      : (instance.persona_id ?? personasStore.baseId);
  }
  return persona?.id ?? personasStore.baseId;
});

// Follow the route: `#/chat/<id>` answers as that instance once the catalog has loaded.
$effect(() => {
  const param = router.param;
  if (!param || instancesStore.catalog === null) return;
  untrack(() => {
    if (instancesStore.find(param)) {
      chat.choose(`i:${param}`);
    } else {
      toasts.error(t('chat.instance_missing', { id: param }));
      router.replaceParam(null);
    }
  });
});

function chooseTarget(next: string) {
  chat.choose(next);
  router.replaceParam(next.startsWith('i:') ? next.slice(2) : null);
}

const groups = $derived(
  modelsStore.providers
    .map((provider) => ({
      provider,
      references: modelsStore.referencesFor(provider),
    }))
    .filter((group) => group.references.length > 0),
);

// Keep the newest text in view while the reader is at the bottom.
$effect(() => {
  void chat.turns.length;
  const last = chat.turns.at(-1);
  void last?.content;
  void last?.reasoning;
  if (stick && scroller) scroller.scrollTop = scroller.scrollHeight;
});

function onScroll() {
  if (!scroller) return;
  stick =
    scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 80;
}

/** Grows the box with its text, up to a limit, so a long message stays readable while typed. */
function fit() {
  if (!composer) return;
  composer.style.height = 'auto';
  composer.style.height = `${Math.min(composer.scrollHeight, 220)}px`;
}

async function send() {
  const text = draft.trim();
  if (!text || chat.streaming || !model) return;
  draft = '';
  stick = true;
  await tick();
  fit();
  await chat.send(text, { personaId, model });
}

function onKeydown(e: KeyboardEvent) {
  // Enter sends; Shift+Enter adds a line. An IME composition's Enter only confirms the text.
  if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    void send();
  }
}

/**
 * Splits a `<think>` block some models write inline from the answer itself. Reasoning streamed
 * separately takes precedence.
 */
function split(turn: ChatTurn): { reasoning: string; content: string } {
  const content = turn.content;
  const start = content.indexOf('<think>');
  if (start === -1) return { reasoning: turn.reasoning, content };
  const end = content.indexOf('</think>', start);
  const inline = content.slice(start + 7, end === -1 ? undefined : end).trim();
  const rest =
    end === -1
      ? content.slice(0, start)
      : content.slice(0, start) + content.slice(end + 8);
  return { reasoning: turn.reasoning || inline, content: rest.trim() };
}
</script>

<PageHead title={t('nav.chat')}>
  {#snippet sub()}
    <span>{t('chat.sub')}</span>
  {/snippet}
  {#snippet actions()}
    <Button type="button" disabled={chat.turns.length === 0} onclick={() => chat.clear()}>
      <MessageSquarePlus size={16} strokeWidth={2} />
      {t('chat.new')}
    </Button>
  {/snippet}
</PageHead>

<div class="card flex flex-wrap items-end gap-x-5 gap-y-3 px-5 py-4">
  <div class="min-w-0 flex-1 basis-[220px]">
    <label class="label" for="chat-target">{t('chat.target')}</label>
    <Select id="chat-target" value={chat.target} onchange={(e) => chooseTarget(e.currentTarget.value)}>
      <option value="">{t('chat.assistant')}</option>
      {#if instancesStore.instances.length > 0}
        <optgroup label={t('nav.instances')}>
          {#each instancesStore.instances as item (item.id)}
            <option value="i:{item.id}">{item.name}</option>
          {/each}
        </optgroup>
      {/if}
      {#if personasStore.library.length > 0}
        <optgroup label={t('nav.personas')}>
          {#each personasStore.library as item (item.id)}
            <option value="p:{item.id}">{item.name}</option>
          {/each}
        </optgroup>
      {/if}
    </Select>
  </div>
  <div class="min-w-0 flex-1 basis-[220px]">
    <label class="label" for="chat-model">{t('chat.model')}</label>
    <Select id="chat-model" bind:value={chat.model}>
      <option value="">
        {inheritedModel
          ? instance?.model
            ? t('chat.model_instance', { model: inheritedModel })
            : t('chat.model_default', { model: inheritedModel })
          : t('chat.model_none')}
      </option>
      {#each groups as group (group.provider)}
        <optgroup label={group.provider}>
          {#each group.references as reference (reference)}
            <option value={reference}>{reference.slice(group.provider.length + 1)}</option>
          {/each}
        </optgroup>
      {/each}
    </Select>
  </div>
  <span class="flex h-[42px] items-center gap-2.5 text-[14px] font-medium whitespace-nowrap">
    <Switch
      checked={chat.tools}
      label={t('chat.tools')}
      onchange={(next) => (chat.tools = next)}
    />
    {t('chat.tools')}
  </span>
</div>

<section class="card flex min-h-[360px] flex-1 flex-col overflow-hidden">
  <div bind:this={scroller} onscroll={onScroll} class="scroll-thin min-h-0 flex-1 overflow-y-auto px-5 py-6 sm:px-8">
    <div class="mx-auto flex max-w-[760px] flex-col gap-5">
      {#if chat.turns.length === 0}
        <div class="flex flex-col items-center gap-1.5 py-14 text-center">
          <p class="m-0 text-[17px] font-semibold">{t('chat.empty_title', { name: speaker })}</p>
          <p class="m-0 max-w-[52ch] hint">
            {instance ? t('chat.empty_instance') : t('chat.empty_text')}
          </p>
        </div>
      {/if}

      {#each chat.turns as turn, index (turn.id)}
        {#if turn.role === 'user'}
          <div
            class="max-w-[85%] self-end rounded-[20px] rounded-br-[6px] bg-accent-tint px-4 py-2.5 text-[15px] leading-relaxed break-words whitespace-pre-wrap text-fg"
          >
            {turn.content}
          </div>
        {:else}
          {@const parts = split(turn)}
          {@const live = chat.streaming && index === chat.turns.length - 1}
          <div class="flex max-w-[92%] flex-col gap-2">
            <span class="text-[12.5px] font-medium text-fg2">{speaker}</span>
            {#if parts.reasoning}
              <details class="rounded-xl bg-sunk" open={live && !parts.content}>
                <summary class="cursor-pointer px-3.5 py-2 text-[13px] font-medium text-fg2 select-none">
                  {live && !parts.content
                    ? t('chat.thinking')
                    : t('chat.thought', { n: parts.reasoning.length })}
                </summary>
                <div class="scroll-thin max-h-64 overflow-y-auto border-t border-line px-3.5 py-2.5 text-[13px] leading-relaxed whitespace-pre-wrap text-fg2">
                  {parts.reasoning}
                </div>
              </details>
            {/if}
            {#if parts.content}
              <div class="text-[15px] leading-relaxed break-words whitespace-pre-wrap text-fg">{parts.content}</div>
            {:else if live}
              <span class="flex items-center gap-2 text-[13.5px] text-fg2">
                <i class="dot animate-pulse bg-accent!"></i>
                {parts.reasoning ? t('chat.writing') : t('chat.thinking')}
              </span>
            {:else if !turn.error}
              <span class="text-[13.5px] text-fg3">{t('chat.no_text')}</span>
            {/if}
            {#if turn.error}
              <div class="notice notice-bad"><span class="min-w-0 break-words">{t('chat.failed', { error: turn.error })}</span></div>
            {/if}
          </div>
        {/if}
      {/each}
    </div>
  </div>

  <form
    class="px-4 pt-1 pb-3 sm:px-6"
    onsubmit={(e) => {
      e.preventDefault();
      void send();
    }}
  >
    <div class="mx-auto flex max-w-[760px] flex-col gap-2.5">
      {#if !model}
        <div class="notice notice-warn items-center">
          <span class="min-w-0 flex-1">{t('chat.no_model')}</span>
          <Button type="button" size="sm" onclick={() => router.navigate('models')}>
            {t('chat.open_models')}
          </Button>
        </div>
      {/if}
      <div class="flex items-end gap-2.5">
        <textarea
          bind:this={composer}
          bind:value={draft}
          rows="1"
          aria-label={t('chat.placeholder', { name: speaker })}
          placeholder={t('chat.placeholder', { name: speaker })}
          oninput={fit}
          onkeydown={onKeydown}
          class="input h-[46px] min-h-[46px] resize-none rounded-[23px] px-5 py-[11px] leading-[1.5] shadow-none focus:shadow-[inset_0_0_0_2px_var(--k-accent)]"
        ></textarea>
        {#if chat.streaming}
          <Button
            type="button"
            square
            class="kanon-btn-46 shrink-0"
            title={t('chat.stop')}
            aria-label={t('chat.stop')}
            onclick={() => chat.stop()}
          >
            <Square size={16} strokeWidth={2} class="fill-current" />
          </Button>
        {:else}
          <Button
            type="submit"
            variant="filled"
            square
            class="kanon-btn-46 shrink-0"
            title={t('chat.send')}
            aria-label={t('chat.send')}
            disabled={!draft.trim() || !model}
          >
            <ArrowUp size={19} strokeWidth={2.2} />
          </Button>
        {/if}
      </div>
      <p class="m-0 text-[12.5px] text-fg3">
        {instance ? t('chat.instance_note') : t('chat.enter_hint')}
      </p>
    </div>
  </form>
</section>
