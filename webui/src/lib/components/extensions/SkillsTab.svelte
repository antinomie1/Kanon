<script lang="ts">
import { BookOpen, RefreshCw, Trash2, Upload } from 'lucide-svelte';
import { untrack } from 'svelte';
import { api } from '../../api/client';
import { errorText } from '../../format';
import { confirmDialog } from '../../stores/confirm.svelte';
import { t } from '../../stores/i18n.svelte';
import { toasts } from '../../stores/toast.svelte';
import type { SkillItem } from '../../types';
import EmptyState from '../ui/EmptyState.svelte';
import Modal from '../ui/Modal.svelte';
import Seg from '../ui/Seg.svelte';
import Switch from '../ui/Switch.svelte';

/**
 * Skills: instruction bundles the model reads when it decides one is relevant.
 *
 * Only a skill's name and description sit in the prompt; the body is fetched on demand, so an
 * installed skill costs context only when it is actually used.
 */

let skills = $state<SkillItem[]>([]);
let loaded = $state(false);
let loading = $state(false);
let error = $state<string | null>(null);
let busy = $state<Record<string, boolean>>({});

let installOpen = $state(false);
let source = $state<'archive' | 'path'>('archive');
let file = $state<File | null>(null);
let path = $state('');
let customId = $state('');
let installing = $state(false);
let installError = $state<string | null>(null);

async function load() {
  loading = true;
  error = null;
  try {
    skills = (await api.getSkills()).skills;
  } catch (e) {
    error = errorText(e);
  } finally {
    loading = false;
    loaded = true;
  }
}

$effect(() => {
  untrack(() => void load());
});

function openInstall() {
  source = 'archive';
  file = null;
  path = '';
  customId = '';
  installError = null;
  installOpen = true;
}

const ready = $derived(
  source === 'archive' ? file !== null : path.trim() !== '',
);

async function install() {
  if (!ready) return;
  installing = true;
  installError = null;
  try {
    const id = customId.trim() || undefined;
    const installed =
      source === 'archive'
        ? await api.installSkillArchive(file as File, id)
        : await api.installSkillPath(path.trim(), id);
    toasts.ok(
      t('extensions.installed_toast', { name: installed.name || installed.id }),
    );
    installOpen = false;
    await load();
  } catch (e) {
    installError = errorText(e);
  } finally {
    installing = false;
  }
}

async function setEnabled(skill: SkillItem, next: boolean) {
  busy = { ...busy, [skill.id]: true };
  try {
    await api.setSkillEnabled(skill.id, next);
    toasts.ok(
      t(next ? 'extensions.on_toast' : 'extensions.off_toast', {
        name: skill.name,
      }),
    );
    await load();
  } catch (e) {
    toasts.error(
      t('extensions.toggle_failed', { name: skill.name, error: errorText(e) }),
    );
  } finally {
    busy = { ...busy, [skill.id]: false };
  }
}

async function remove(skill: SkillItem) {
  const yes = await confirmDialog({
    title: t('extensions.skill_remove_title', { name: skill.name }),
    message: t('extensions.skill_remove_text'),
    confirm: t('extensions.remove'),
    danger: true,
  });
  if (!yes) return;
  busy = { ...busy, [skill.id]: true };
  try {
    await api.removeSkill(skill.id);
    toasts.ok(t('extensions.removed_toast', { name: skill.name }));
    await load();
  } catch (e) {
    toasts.error(errorText(e));
  } finally {
    busy = { ...busy, [skill.id]: false };
  }
}
</script>

<div class="flex flex-wrap items-center justify-between gap-3 px-1">
  <p class="m-0 max-w-[68ch] hint">{t('extensions.skills_hint')}</p>
  <div class="flex flex-wrap gap-2.5">
    <button type="button" class="btn" disabled={loading} onclick={() => void load()}>
      <RefreshCw size={16} strokeWidth={2} class={loading ? 'animate-spin' : ''} />
      {t('platforms.refresh')}
    </button>
    <button type="button" class="btn btn-primary" onclick={openInstall}>
      <Upload size={16} strokeWidth={2} />
      {t('extensions.skill_install')}
    </button>
  </div>
</div>

{#if error}
  <div class="notice notice-bad">{error}</div>
{/if}

{#if loaded && skills.length === 0 && !error}
  <div class="card">
    <EmptyState icon={BookOpen} title={t('extensions.skills_empty')} text={t('extensions.skills_empty_text')}>
      {#snippet action()}
        <button type="button" class="btn btn-primary" onclick={openInstall}>
          <Upload size={16} strokeWidth={2} />
          {t('extensions.skill_install')}
        </button>
      {/snippet}
    </EmptyState>
  </div>
{:else if skills.length > 0}
  <ul class="group-list m-0 list-none p-0">
    {#each skills as skill (skill.id)}
      <li class="flex flex-wrap items-center gap-x-6 gap-y-2 py-4">
        <div class="min-w-0 flex-1 basis-[320px]">
          <div class="flex flex-wrap items-baseline gap-x-2.5">
            <h2 class="m-0 text-[16px] font-semibold {skill.enabled ? '' : 'text-fg2'}">{skill.name}</h2>
            <span class="text-[12.5px] text-fg3">{skill.id}</span>
          </div>
          <p class="m-0 mt-0.5 max-w-[80ch] text-[13.5px] text-fg2">{skill.description}</p>
        </div>
        <div class="ml-auto flex items-center gap-2.5">
          <button
            type="button"
            class="btn btn-sm btn-quiet btn-icon"
            aria-label={t('extensions.skill_remove_title', { name: skill.name })}
            title={t('extensions.remove')}
            disabled={busy[skill.id]}
            onclick={() => void remove(skill)}
          >
            <Trash2 size={16} strokeWidth={2} />
          </button>
          <Switch
            checked={skill.enabled}
            disabled={busy[skill.id]}
            label={skill.enabled
              ? t('platforms.turn_off', { name: skill.name })
              : t('platforms.turn_on', { name: skill.name })}
            onchange={(next) => void setEnabled(skill, next)}
          />
        </div>
      </li>
    {/each}
  </ul>
{:else if !error}
  <p class="m-0 px-1 hint">{t('common.loading')}</p>
{/if}

<Modal open={installOpen} title={t('extensions.skill_install')} locked={installing} onclose={() => (installOpen = false)}>
  <form
    id="install-skill"
    class="flex flex-col gap-4"
    onsubmit={(e) => {
      e.preventDefault();
      void install();
    }}
  >
    <Seg
      label={t('extensions.install_from')}
      value={source}
      onchange={(next: 'archive' | 'path') => {
        source = next;
        installError = null;
      }}
      options={[
        { value: 'archive', label: t('extensions.from_zip') },
        { value: 'path', label: t('extensions.from_path') },
      ]}
    />
    {#if source === 'archive'}
      <div>
        <label class="label" for="skill-file">{t('extensions.skill_archive')}</label>
        <input
          id="skill-file"
          type="file"
          accept=".zip,application/zip"
          class="block w-full text-[14px] text-fg2 file:mr-3 file:h-[34px] file:cursor-pointer file:rounded-full file:border-0 file:bg-sunk file:px-4 file:font-medium file:text-fg"
          onchange={(e) => (file = e.currentTarget.files?.[0] ?? null)}
        />
        <p class="m-0 mt-2 hint">{t('extensions.skill_archive_hint')}</p>
      </div>
    {:else}
      <div>
        <label class="label" for="skill-path">{t('extensions.path_label')}</label>
        <input id="skill-path" class="input mono" spellcheck="false" placeholder="./my-skill" bind:value={path} />
        <p class="m-0 mt-2 hint">{t('extensions.skill_path_hint')}</p>
      </div>
    {/if}
    <div>
      <label class="label" for="skill-id">{t('extensions.skill_id')}</label>
      <input id="skill-id" class="input mono" spellcheck="false" placeholder="my-skill" bind:value={customId} />
      <p class="m-0 mt-2 hint">{t('extensions.skill_id_hint')}</p>
    </div>
    {#if installError}
      <div class="notice notice-bad"><span class="min-w-0 break-words">{installError}</span></div>
    {/if}
  </form>

  {#snippet footer()}
    <button type="button" class="btn" disabled={installing} onclick={() => (installOpen = false)}>
      {t('common.cancel')}
    </button>
    <button type="submit" form="install-skill" class="btn btn-primary" disabled={!ready || installing}>
      {installing ? t('extensions.installing') : t('extensions.install')}
    </button>
  {/snippet}
</Modal>
