<script lang="ts">
import { onMount } from 'svelte';
import { api } from '../../api/client';
import { errorText } from '../../format';
import { t } from '../../stores/i18n.svelte';

let { sessionId, attachmentId }: { sessionId: string; attachmentId: string } = $props();
let source = $state('');
let error = $state<string | null>(null);

onMount(() => {
  let disposed = false;
  void api.getDshAttachment(sessionId, attachmentId).then(image => {
    if (disposed) return;
    if (!image.attachment.mediaType.startsWith('image/')) throw new Error('Invalid DSH image media type');
    source = `data:${image.attachment.mediaType};base64,${image.data}`;
  }).catch(e => { if (!disposed) error = errorText(e); });
  return () => { disposed = true; };
});
</script>

{#if source}
  <img alt={t('dsh.image')} src={source} class="max-h-64 max-w-full" />
{:else if error}
  <p class="notice notice-bad">{error}</p>
{:else}
  <p class="hint">{t('common.loading')}</p>
{/if}
