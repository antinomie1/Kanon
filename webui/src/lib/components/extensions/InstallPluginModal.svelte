<script lang="ts">
import { api } from '../../api/client';
import { errorText } from '../../format';
import { t } from '../../stores/i18n.svelte';
import { toasts } from '../../stores/toast.svelte';
import Button from '../ui/Button.svelte';
import Modal from '../ui/Modal.svelte';
import Seg from '../ui/Seg.svelte';
import TextField from '../ui/TextField.svelte';

/**
 * Installs a plugin from a directory on the node or from an uploaded package.
 *
 * Installing does not fetch dependencies: a Python or TypeScript plugin brings its own
 * environment, prepared in its directory with the language's own tools.
 */
let {
  open,
  onclose,
  oninstalled,
}: {
  open: boolean;
  onclose: () => void;
  /** Called after the node accepted the plugin, so the list can reload. */
  oninstalled: () => void;
} = $props();

let source = $state<'path' | 'archive'>('path');
let path = $state('');
let file = $state<File | null>(null);
let installing = $state(false);
let error = $state<string | null>(null);

// Every opening starts from an empty form; a previous failure is not worth keeping.
$effect(() => {
  if (open) {
    source = 'path';
    path = '';
    file = null;
    error = null;
  }
});

const ready = $derived(source === 'path' ? path.trim() !== '' : file !== null);

async function install() {
  if (!ready) return;
  installing = true;
  error = null;
  try {
    const res =
      source === 'path'
        ? await api.installPluginPath(path.trim())
        : await api.installPluginArchive(file as File);
    toasts.ok(
      t('extensions.installed_toast', { name: res.name || res.plugin_id }),
    );
    oninstalled();
    onclose();
  } catch (e) {
    error = errorText(e);
  } finally {
    installing = false;
  }
}
</script>

<Modal {open} {onclose} title={t('extensions.install_plugin')} locked={installing}>
  <form
    id="install-plugin"
    class="flex flex-col gap-4"
    onsubmit={(e) => {
      e.preventDefault();
      void install();
    }}
  >
    <Seg
      label={t('extensions.install_from')}
      value={source}
      onchange={(next: 'path' | 'archive') => {
        source = next;
        error = null;
      }}
      options={[
        { value: 'path', label: t('extensions.from_path') },
        { value: 'archive', label: t('extensions.from_archive') },
      ]}
    />
    {#if source === 'path'}
      <div>
        <label class="label" for="plugin-path">{t('extensions.path_label')}</label>
        <TextField
          id="plugin-path"
          mono
          spellcheck="false"
          placeholder="./plugins/demo_weather"
          bind:value={path}
        />
        <p class="m-0 mt-2 hint">{t('extensions.path_hint')}</p>
      </div>
    {:else}
      <div>
        <label class="label" for="plugin-file">{t('extensions.archive_label')}</label>
        <input
          id="plugin-file"
          type="file"
          accept=".kpk,.zip,application/zip"
          class="block w-full text-[14px] text-fg2 file:mr-3 file:h-[34px] file:cursor-pointer file:rounded-full file:border-0 file:bg-sunk file:px-4 file:font-medium file:text-fg"
          onchange={(e) => (file = e.currentTarget.files?.[0] ?? null)}
        />
        <p class="m-0 mt-2 hint">{t('extensions.archive_hint')}</p>
      </div>
    {/if}
    {#if error}
      <div class="notice notice-bad"><span class="min-w-0 break-words">{error}</span></div>
    {/if}
  </form>

  {#snippet footer()}
    <Button type="button" disabled={installing} onclick={onclose}>{t('common.cancel')}</Button>
    <Button type="submit" form="install-plugin" variant="filled" disabled={!ready || installing}>
      {installing ? t('extensions.installing') : t('extensions.install')}
    </Button>
  {/snippet}
</Modal>
