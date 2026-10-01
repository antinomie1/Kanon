<script lang="ts">
import { confirmStore } from '../../stores/confirm.svelte';
import { t } from '../../stores/i18n.svelte';
import Button from './Button.svelte';
import Modal from './Modal.svelte';

const pending = $derived(confirmStore.pending);
</script>

<Modal
  open={pending !== null}
  title={pending?.title ?? ''}
  width="max-w-md"
  onclose={() => confirmStore.answer(false)}
>
  {#if pending?.message}
    <p class="m-0 text-[14.5px] leading-relaxed text-fg2">{pending.message}</p>
  {/if}
  {#snippet footer()}
    <Button type="button" onclick={() => confirmStore.answer(false)}>
      {pending?.cancel ?? t('common.cancel')}
    </Button>
    <Button
      type="button"
      variant={pending?.danger ? 'danger-filled' : 'filled'}
      onclick={() => confirmStore.answer(true)}
    >
      {pending?.confirm ?? ''}
    </Button>
  {/snippet}
</Modal>
