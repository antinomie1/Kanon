<script lang="ts" module>
/**
 * Open dialogs, innermost last. A dialog opened from inside another one (a QR code from a
 * settings drawer) must be the only one that answers Escape; without this every open dialog
 * would close on the same key press.
 */
const openStack: symbol[] = [];
</script>

<script lang="ts">
import { X } from 'lucide-svelte';
import type { Snippet } from 'svelte';
import { t } from '../../stores/i18n.svelte';

/**
 * Dialog or side drawer with a backdrop.
 *
 * Escape and a click on the backdrop both close it, unless `locked` is set while something is
 * being saved, so a half-finished request never loses its window.
 */
let {
  open,
  title,
  onclose,
  children,
  footer,
  variant = 'dialog',
  width = 'max-w-xl',
  locked = false,
}: {
  open: boolean;
  title: string;
  onclose: () => void;
  children: Snippet;
  footer?: Snippet;
  variant?: 'dialog' | 'drawer';
  /** Tailwind max-width class of the panel. */
  width?: string;
  locked?: boolean;
} = $props();

let panel = $state<HTMLDivElement>();
const self = Symbol('modal');

$effect(() => {
  if (!open) return;
  openStack.push(self);
  return () => {
    const index = openStack.lastIndexOf(self);
    if (index >= 0) openStack.splice(index, 1);
  };
});

// Move focus into the panel when it opens so keyboard users land inside it, and give it back to
// whatever opened it when it closes.
$effect(() => {
  if (!open || !panel) return;
  const opener = document.activeElement as HTMLElement | null;
  const first = panel.querySelector<HTMLElement>(
    'input:not([disabled]), textarea:not([disabled]), select:not([disabled]), button:not([disabled]):not([data-close])',
  );
  (first ?? panel).focus();
  return () => opener?.focus?.();
});

function close() {
  if (!locked) onclose();
}

function onkeydown(e: KeyboardEvent) {
  if (open && e.key === 'Escape' && openStack.at(-1) === self) {
    e.stopPropagation();
    close();
  }
}
</script>

<svelte:window {onkeydown} />

{#if open}
  <div
    class="fixed inset-0 z-50 flex bg-[rgb(20_18_30/0.38)] {variant === 'drawer'
      ? 'justify-end'
      : 'items-start justify-center overflow-y-auto px-4 py-[8vh]'}"
  >
    <button
      type="button"
      class="absolute inset-0 cursor-default"
      aria-label={t('common.close')}
      tabindex="-1"
      onclick={close}
    ></button>
    <div
      bind:this={panel}
      role="dialog"
      aria-modal="true"
      aria-label={title}
      tabindex="-1"
      class="relative flex w-full flex-col bg-card shadow-[var(--k-pop)] outline-none {width} {variant ===
      'drawer'
        ? 'h-full'
        : 'max-h-[84vh] rounded-[22px]'}"
    >
      <div class="flex items-center gap-3 px-6 pt-5 pb-3">
        <h2 class="m-0 min-w-0 flex-1 truncate text-[18px] font-extrabold">{title}</h2>
        <button
          type="button"
          data-close
          class="btn btn-quiet btn-icon btn-sm"
          aria-label={t('common.close')}
          disabled={locked}
          onclick={close}
        >
          <X size={18} strokeWidth={2.4} />
        </button>
      </div>
      <div class="scroll-thin min-h-0 flex-1 overflow-y-auto px-6 pb-6">
        {@render children()}
      </div>
      {#if footer}
        <div class="flex flex-wrap items-center justify-end gap-2.5 border-t border-line px-6 py-4">
          {@render footer()}
        </div>
      {/if}
    </div>
  </div>
{/if}
