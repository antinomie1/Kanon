<script lang="ts">
import { CircleAlert, CircleCheck, Info, X } from 'lucide-svelte';
import { risen } from '../../motion';
import { t } from '../../stores/i18n.svelte';
import { toasts } from '../../stores/toast.svelte';
import Button from './Button.svelte';

// Toasts are Material snackbars: an inverse surface (dark on a light page, light on a dark one)
// with small corners and the action in inverse-primary. Icons take the snackbar's own text colour
// because the status colours do not hold contrast on an inverse surface; the icon's shape still
// tells the outcome apart.
</script>

<div
  class="pointer-events-none fixed right-4 bottom-4 z-[60] flex w-[calc(100%-2rem)] max-w-md flex-col items-end gap-2 sm:right-8 sm:bottom-6"
  aria-live="polite"
>
  {#each toasts.items as toast (toast.id)}
    <div
      class="pointer-events-auto flex max-w-full items-center gap-3 rounded-[4px] bg-bar py-1.5 pr-2 pl-4 text-[14px] text-on-bar shadow-[var(--k-pop)]"
      role={toast.tone === 'bad' ? 'alert' : 'status'}
      in:risen
      out:risen={{ duration: 120 }}
    >
      {#if toast.tone === 'ok'}
        <CircleCheck size={17} strokeWidth={2} class="shrink-0" />
      {:else if toast.tone === 'bad'}
        <CircleAlert size={17} strokeWidth={2} class="shrink-0" />
      {:else}
        <Info size={17} strokeWidth={2} class="shrink-0" />
      {/if}
      <span class="min-w-0 py-1.5 leading-snug">{toast.message}</span>
      {#if toast.action}
        {@const action = toast.action}
        <Button
          type="button"
          variant="inverse"
          size="sm"
          onclick={() => {
            toasts.dismiss(toast.id);
            action.run();
          }}
        >
          {action.label}
        </Button>
      {/if}
      <Button
        type="button"
        variant="inverse-plain"
        size="xs"
        square
        class="opacity-75 hover:opacity-100"
        aria-label={t('common.close')}
        onclick={() => toasts.dismiss(toast.id)}
      >
        <X size={15} strokeWidth={2} />
      </Button>
    </div>
  {/each}
</div>
