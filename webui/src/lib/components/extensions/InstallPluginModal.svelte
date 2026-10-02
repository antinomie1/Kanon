<script lang="ts">
import { ApiError, api } from '../../api/client';
import { errorText } from '../../format';
import { t } from '../../stores/i18n.svelte';
import { toasts } from '../../stores/toast.svelte';
import Button from '../ui/Button.svelte';
import Modal from '../ui/Modal.svelte';
import Seg from '../ui/Seg.svelte';
import TextField from '../ui/TextField.svelte';

/**
 * Installs a plugin from a folder on the node, an uploaded package, a package URL or a Git
 * repository.
 *
 * Every source goes through the node's one installer, which checks the manifest and the plugin's
 * `kanon_version` and never replaces an installed plugin unless asked. When the node answers that
 * the plugin is already installed (`409`), the dialog offers to replace it instead of failing, so an
 * upgrade is one deliberate extra click. Dependencies are installed by the node with the plugin's
 * own tool (`uv`, `bun`, `npm`) when it launches the plugin.
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

type Source = 'path' | 'archive' | 'url' | 'git';

let source = $state<Source>('path');
let path = $state('');
let url = $state('');
let git = $state('');
let gitRef = $state('');
let file = $state<File | null>(null);
let installing = $state(false);
let error = $state<string | null>(null);
/** The node refused because the plugin is installed; the next install replaces it. */
let conflict = $state(false);

// Every opening starts from an empty form; a previous failure is not worth keeping.
$effect(() => {
  if (open) {
    source = 'path';
    path = '';
    url = '';
    git = '';
    gitRef = '';
    file = null;
    error = null;
    conflict = false;
  }
});

const ready = $derived(
  source === 'path'
    ? path.trim() !== ''
    : source === 'url'
      ? url.trim() !== ''
      : source === 'git'
        ? git.trim() !== ''
        : file !== null,
);

function send(replace: boolean) {
  switch (source) {
    case 'archive':
      return api.installPluginArchive(file as File, replace);
    case 'url':
      return api.installPlugin({ url: url.trim(), replace });
    case 'git':
      return api.installPlugin({
        git: git.trim(),
        ref: gitRef.trim() || undefined,
        replace,
      });
    default:
      return api.installPlugin({ path: path.trim(), replace });
  }
}

async function install() {
  if (!ready) return;
  installing = true;
  error = null;
  const replace = conflict;
  try {
    const res = await send(replace);
    const name = res.name || res.plugin_id;
    if (res.status === 'RuntimeUnavailable') {
      toasts.error(
        t('extensions.installed_no_runtime', {
          name,
          reason: res.message ?? '',
        }),
      );
    } else {
      toasts.ok(
        t(
          replace ? 'extensions.replaced_toast' : 'extensions.installed_toast',
          { name },
        ),
      );
    }
    oninstalled();
    onclose();
  } catch (e) {
    conflict = e instanceof ApiError && e.status === 409;
    error = errorText(e);
  } finally {
    installing = false;
  }
}

function pick(next: Source) {
  source = next;
  error = null;
  conflict = false;
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
    <div class="scroll-thin overflow-x-auto overflow-y-hidden">
      <Seg
        label={t('extensions.install_from')}
        value={source}
        onchange={pick}
        options={[
          { value: 'path', label: t('extensions.from_path') },
          { value: 'archive', label: t('extensions.from_archive') },
          { value: 'url', label: t('extensions.from_url') },
          { value: 'git', label: 'Git' },
        ]}
      />
    </div>
    {#if source === 'path'}
      <div>
        <label class="label" for="plugin-path">{t('extensions.path_label')}</label>
        <TextField
          id="plugin-path"
          class="w-full"
          mono
          spellcheck="false"
          placeholder="./plugins/demo_weather"
          bind:value={path}
          oninput={() => (conflict = false)}
        />
        <p class="m-0 mt-2 hint">{t('extensions.path_hint')}</p>
      </div>
    {:else if source === 'url'}
      <div>
        <label class="label" for="plugin-url">{t('extensions.url_label')}</label>
        <TextField
          id="plugin-url"
          class="w-full"
          mono
          spellcheck="false"
          placeholder="https://example.org/weather-1.2.0.kpk"
          bind:value={url}
          oninput={() => (conflict = false)}
        />
        <p class="m-0 mt-2 hint">{t('extensions.url_hint')}</p>
      </div>
    {:else if source === 'git'}
      <div>
        <label class="label" for="plugin-git">{t('extensions.git_label')}</label>
        <TextField
          id="plugin-git"
          class="w-full"
          mono
          spellcheck="false"
          placeholder="https://github.com/example/kanon-weather.git"
          bind:value={git}
          oninput={() => (conflict = false)}
        />
      </div>
      <div>
        <label class="label" for="plugin-git-ref">{t('extensions.git_ref_label')}</label>
        <TextField id="plugin-git-ref" class="w-full" mono spellcheck="false" placeholder="main" bind:value={gitRef} />
        <p class="m-0 mt-2 hint">{t('extensions.git_hint')}</p>
      </div>
    {:else}
      <div>
        <label class="label" for="plugin-file">{t('extensions.archive_label')}</label>
        <input
          id="plugin-file"
          type="file"
          accept=".kpk,.zip,application/zip"
          class="block w-full text-[14px] text-fg2 file:mr-3 file:h-[34px] file:cursor-pointer file:rounded-full file:border-0 file:bg-sunk file:px-4 file:font-medium file:text-fg"
          onchange={(e) => {
            file = e.currentTarget.files?.[0] ?? null;
            conflict = false;
          }}
        />
        <p class="m-0 mt-2 hint">{t('extensions.archive_hint')}</p>
      </div>
    {/if}
    {#if error}
      <div class="notice {conflict ? 'notice-warn' : 'notice-bad'}">
        <span class="min-w-0 break-words">
          {error}
          {#if conflict}<br />{t('extensions.replace_hint')}{/if}
        </span>
      </div>
    {/if}
  </form>

  {#snippet footer()}
    <Button type="button" disabled={installing} onclick={onclose}>{t('common.cancel')}</Button>
    <Button type="submit" form="install-plugin" variant="filled" disabled={!ready || installing}>
      {installing
        ? t('extensions.installing')
        : conflict
          ? t('extensions.replace')
          : t('extensions.install')}
    </Button>
  {/snippet}
</Modal>
