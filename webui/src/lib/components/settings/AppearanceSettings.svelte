<script lang="ts">
import { Check, Monitor, Moon, Plus, Sun } from 'lucide-svelte';
import { i18n, type Locale, t } from '../../stores/i18n.svelte';
import {
  ACCENTS,
  type Accent,
  type ThemeMode,
  theme,
} from '../../stores/theme.svelte';
import Section from '../ui/Section.svelte';
import Seg from '../ui/Seg.svelte';
import Switch from '../ui/Switch.svelte';

// Swatch colours shown in the picker. Each is the seed its Material scheme in `app.css` is
// generated from, so it names the accent family rather than matching any one role exactly.
const SWATCH: Record<Accent, string> = {
  violet: '#5b57e0',
  blue: '#2a66db',
  teal: '#0b7d70',
  coral: '#c94a22',
  rose: '#c23a6e',
  graphite: '#3d3a48',
};

let previewOn = $state(true);
</script>

<Section title={t('settings.theme')} hint={t('settings.theme_hint')}>
  <div>
    <Seg
      label={t('settings.theme')}
      value={theme.currentMode}
      onchange={(next: ThemeMode) => theme.setMode(next)}
      options={[
        { value: 'system', icon: Monitor, label: t('settings.theme_system') },
        { value: 'light', icon: Sun, label: t('settings.theme_light') },
        { value: 'dark', icon: Moon, label: t('settings.theme_dark') },
      ]}
    />
  </div>
</Section>

<Section title={t('settings.accent')} hint={t('settings.accent_hint')}>
  <div class="flex flex-wrap gap-2" role="radiogroup" aria-label={t('settings.accent')}>
    {#each ACCENTS as accent (accent)}
      {@const on = theme.accent === accent}
      <button
        type="button"
        role="radio"
        aria-checked={on}
        onclick={() => theme.setAccent(accent)}
        class="inline-flex h-10 items-center gap-2 rounded-full pr-4 pl-2 text-[14px] font-medium whitespace-nowrap {on
          ? 'bg-accent-tint text-accent-fg shadow-[inset_0_0_0_2px_var(--k-accent)]'
          : 'bg-sunk text-fg hover:text-fg'}"
      >
        <i
          class="grid h-6 w-6 place-items-center rounded-full text-white"
          style="background: {SWATCH[accent]}"
        >
          {#if on}<Check size={14} strokeWidth={2.6} />{/if}
        </i>
        {t(`settings.accent_${accent}`)}
      </button>
    {/each}
  </div>
  <div class="flex flex-wrap items-center gap-4 self-start rounded-2xl px-4 py-3.5 shadow-[inset_0_0_0_1px_var(--k-line)]">
    <span class="text-[13px] whitespace-nowrap text-fg2">{t('settings.preview')}</span>
    <button type="button" class="btn btn-primary btn-sm" tabindex="-1">
      <Plus size={14} strokeWidth={2.2} />
      {t('instances.new')}
    </button>
    <Switch checked={previewOn} label={t('settings.preview')} onchange={(next) => (previewOn = next)} />
    <span class="text-[14px] font-medium whitespace-nowrap text-accent">{t('nav.instances')}</span>
  </div>
</Section>

<Section title={t('common.language')} hint={t('settings.language_hint')}>
  <div>
    <Seg
      label={t('common.language')}
      value={i18n.locale}
      onchange={(next: Locale) => i18n.setLocale(next)}
      options={[
        { value: 'zh', label: '中文' },
        { value: 'en', label: 'English' },
      ]}
    />
  </div>
</Section>
