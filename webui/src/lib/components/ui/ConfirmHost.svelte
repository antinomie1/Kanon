<script lang="ts">
import { confirmStore } from '../../stores/confirm.svelte';
import { t } from '../../stores/i18n.svelte';
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
    <button type="button" class="btn" onclick={() => confirmStore.answer(false)}>
      {pending?.cancel ?? t('common.cancel')}
    </button>
    <button
      type="button"
      class="btn {pending?.danger ? 'btn-danger-solid' : 'btn-primary'}"
      onclick={() => confirmStore.answer(true)}
    >
      {pending?.confirm ?? ''}
    </button>
  {/snippet}
</Modal>
