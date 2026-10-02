<script lang="ts">
import { ExternalLink } from 'lucide-svelte';
import { t } from '../../stores/i18n.svelte';
import Button from '../ui/Button.svelte';
import Modal from '../ui/Modal.svelte';

/**
 * A plugin's own console page (`pages/index.html` in its folder), shown in a sandboxed frame.
 *
 * The frame never gets `allow-same-origin`: the page is served from the console's origin, and
 * without the sandbox it could read the console's storage and drive it. The node also sends
 * `Content-Security-Policy: sandbox` with every plugin response, so the page stays isolated even
 * when opened in its own tab. Scripts and forms are allowed so a page can be an application that
 * talks to the plugin's own HTTP routes (`../http/...`).
 */
let {
  pluginId,
  name,
  onclose,
}: {
  /** Plugin whose page is shown; `null` keeps the dialog closed. */
  pluginId: string | null;
  name: string;
  onclose: () => void;
} = $props();

const src = $derived(
  pluginId ? `/api/v1/plugins/${encodeURIComponent(pluginId)}/pages/` : '',
);
</script>

<Modal
  open={pluginId !== null}
  title={t('extensions.page_title', { name })}
  width="max-w-5xl"
  {onclose}
>
  {#if pluginId}
    <iframe
      title={t('extensions.page_title', { name })}
      {src}
      sandbox="allow-scripts allow-forms allow-popups allow-modals allow-downloads"
      referrerpolicy="no-referrer"
      class="block h-[70vh] w-full rounded-xl border border-line bg-card"
    ></iframe>
  {/if}

  {#snippet footer()}
    <Button href={src} target="_blank" rel="noopener noreferrer" variant="text" class="no-underline">
      <ExternalLink size={15} strokeWidth={2} />
      {t('extensions.page_new_tab')}
    </Button>
    <Button type="button" onclick={onclose}>{t('common.close')}</Button>
  {/snippet}
</Modal>
