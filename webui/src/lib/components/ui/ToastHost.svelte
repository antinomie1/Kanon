<script lang="ts">
import { CircleAlert, CircleCheck, Info, X } from 'lucide-svelte';
import { t } from '../../stores/i18n.svelte';
import { toasts } from '../../stores/toast.svelte';
</script>

<div
  class="pointer-events-none fixed right-4 bottom-4 z-[60] flex w-[calc(100%-2rem)] max-w-md flex-col items-end gap-2 sm:right-8 sm:bottom-6"
  aria-live="polite"
>
  {#each toasts.items as toast (toast.id)}
    <div
      class="pointer-events-auto flex max-w-full items-center gap-3 rounded-[22px] bg-card py-2 pr-2 pl-4 text-[14px] font-bold shadow-[var(--k-pop),0_0_0_1px_var(--k-line)]"
      role={toast.tone === 'bad' ? 'alert' : 'status'}
    >
      {#if toast.tone === 'ok'}
        <CircleCheck size={17} strokeWidth={2.4} class="shrink-0 text-ok" />
      {:else if toast.tone === 'bad'}
        <CircleAlert size={17} strokeWidth={2.4} class="shrink-0 text-danger" />
      {:else}
        <Info size={17} strokeWidth={2.4} class="shrink-0 text-fg2" />
      {/if}
      <span class="min-w-0 py-1.5 leading-snug">{toast.message}</span>
      {#if toast.action}
        {@const action = toast.action}
        <button
          type="button"
          class="btn btn-sm"
          onclick={() => {
            toasts.dismiss(toast.id);
            action.run();
          }}
        >
          {action.label}
        </button>
      {/if}
      <button
        type="button"
        class="btn btn-quiet btn-icon btn-xs"
        aria-label={t('common.close')}
        onclick={() => toasts.dismiss(toast.id)}
      >
        <X size={15} strokeWidth={2.4} />
      </button>
    </div>
  {/each}
</div>
