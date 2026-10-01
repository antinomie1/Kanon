<script lang="ts">
import { TriangleAlert } from 'lucide-svelte';
import { api } from '../../api/client';
import { errorText } from '../../format';
import { confirmDialog } from '../../stores/confirm.svelte';
import { t } from '../../stores/i18n.svelte';
import { toasts } from '../../stores/toast.svelte';
import type { BashPolicy } from '../../types';
import Section from '../ui/Section.svelte';
import Seg from '../ui/Seg.svelte';
import Switch from '../ui/Switch.svelte';

/**
 * Node-wide Bash settings. Who may use Bash comes from each instance's command permissions, and
 * where from each instance's Bash scope; this is the master switch and the execution backend.
 */

let saved = $state<BashPolicy | null>(null);
let draft = $state<BashPolicy | null>(null);
let loadError = $state<string | null>(null);
let saving = $state(false);

$effect(() => {
  api
    .getBashPolicy()
    .then((policy) => {
      saved = policy;
      draft = structuredClone(policy);
    })
    .catch((e) => (loadError = errorText(e)));
});

/** The review model travels as `null` when blank, so compare in that normalized form. */
function normalized(policy: BashPolicy): BashPolicy {
  return {
    ...policy,
    local: {
      ...policy.local,
      review_model: policy.local.review_model?.trim() || null,
    },
  };
}

const dirty = $derived(
  draft !== null &&
    saved !== null &&
    JSON.stringify(normalized(draft)) !== JSON.stringify(normalized(saved)),
);

async function save() {
  if (!draft) return;
  saving = true;
  try {
    const next = await api.setBashPolicy(normalized(draft));
    saved = next;
    draft = structuredClone(next);
    toasts.ok(t('bash.saved'));
  } catch (e) {
    toasts.error(errorText(e));
  } finally {
    saving = false;
  }
}

async function resetSandbox() {
  const yes = await confirmDialog({
    title: t('bash.reset_title'),
    message: t('bash.reset_confirm'),
    confirm: t('bash.reset'),
    danger: true,
  });
  if (!yes) return;
  saving = true;
  try {
    await api.resetBashSandbox();
    toasts.ok(t('bash.reset_done'));
  } catch (e) {
    toasts.error(errorText(e));
  } finally {
    saving = false;
  }
}
</script>

{#if !draft}
  <p class="m-0 py-6 hint">{loadError ?? t('common.loading')}</p>
{:else}
  <Section title={t('bash.title')} hint={t('bash.hint')}>
    <div class="flex items-start gap-3 text-[14.5px]">
      <span class="min-w-0 flex-1">
        <span class="block font-medium">{t('bash.enabled')}</span>
        <span class="block hint">{t('bash.identity_hint')}</span>
      </span>
      <Switch
        checked={draft.enabled}
        disabled={saving}
        label={t('bash.enabled')}
        onchange={(next) => draft && (draft.enabled = next)}
      />
    </div>
  </Section>

  <Section title={t('bash.execution_mode')} hint={t('bash.mode_hint')}>
    <div>
      <Seg
        label={t('bash.execution_mode')}
        value={draft.execution_mode}
        disabled={saving}
        onchange={(next: BashPolicy['execution_mode']) => draft && (draft.execution_mode = next)}
        options={[
          { value: 'sandbox', label: t('bash.mode_sandbox') },
          { value: 'local', label: t('bash.mode_local') },
        ]}
      />
    </div>

    {#if draft.execution_mode === 'local'}
      <div class="notice notice-warn">
        <TriangleAlert size={16} strokeWidth={2} class="mt-0.5 shrink-0" />
        {t('bash.local_hint')}
      </div>
      <label class="block">
        <span class="label">{t('bash.local_workdir')}</span>
        <input bind:value={draft.local.working_dir} disabled={saving} class="input mono" />
      </label>
      <div class="flex items-start gap-3 text-[14.5px]">
        <span class="min-w-0 flex-1">
          <span class="block font-medium">{t('bash.auto_review')}</span>
          <span class="block hint">{t('bash.review_hint')}</span>
        </span>
        <Switch
          checked={draft.local.auto_review}
          disabled={saving}
          label={t('bash.auto_review')}
          onchange={(next) => draft && (draft.local.auto_review = next)}
        />
      </div>
      {#if draft.local.auto_review}
        <label class="block">
          <span class="label">{t('bash.review_model')}</span>
          <input
            value={draft.local.review_model ?? ''}
            oninput={(e) => draft && (draft.local.review_model = e.currentTarget.value)}
            disabled={saving}
            placeholder="provider/model"
            class="input mono"
          />
        </label>
      {/if}
    {:else}
      <p class="m-0 hint">{t('bash.sandbox_hint')}</p>
      <div class="flex items-center gap-3 text-[14.5px]">
        <span class="flex-1 font-medium">{t('bash.network')}</span>
        <Switch
          checked={draft.sandbox.network}
          disabled={saving}
          label={t('bash.network')}
          onchange={(next) => draft && (draft.sandbox.network = next)}
        />
      </div>
      <label class="block">
        <span class="label">{t('bash.image')}</span>
        <input bind:value={draft.sandbox.image} disabled={saving} class="input mono" />
      </label>
      <div class="flex flex-wrap items-center justify-between gap-3">
        <span class="text-[13.5px] text-fg2">
          {t('bash.limits', {
            memory: draft.sandbox.memory_mb,
            cpus: draft.sandbox.cpus,
            pids: draft.sandbox.pids_limit,
          })}
        </span>
        <button type="button" class="btn btn-sm btn-danger" disabled={saving} onclick={resetSandbox}>
          {t('bash.reset')}
        </button>
      </div>
    {/if}
  </Section>

  <div class="flex justify-end gap-2.5 py-5">
    <button
      type="button"
      class="btn"
      disabled={!dirty || saving}
      onclick={() => saved && (draft = structuredClone(saved))}
    >
      {t('instances.discard')}
    </button>
    <button type="button" class="btn btn-primary" disabled={!dirty || saving} onclick={save}>
      {saving ? t('instances.saving') : t('instances.save_changes')}
    </button>
  </div>
{/if}
