<script lang="ts">
import { Eye, EyeOff, QrCode, Save } from 'lucide-svelte';
import { api } from '../../api/client';
import { t } from '../../stores/i18n.svelte';
import type { PluginConfigResponse } from '../../types';
import QqOfficialQrModal from '../adapters/QqOfficialQrModal.svelte';

/**
 * Configuration drawer for one discovered plugin.
 *
 * Plugin configuration is generic JSON validated against the manifest's schema, with a
 * hand-written visual form for the QQ Official adapter because its fields (AppID, secret,
 * Markdown template, intents) are the ones an operator edits by hand. The drawer is shared by the
 * plugins page and the platform-adapters page, so opening it from either place edits exactly the
 * same node state.
 */
let {
  pluginId,
  onclose,
  onrefresh,
}: {
  /** Plugin whose configuration is shown; `null` keeps the drawer closed. */
  pluginId: string | null;
  /** Called when the operator dismisses the drawer. */
  onclose: () => void;
  /** Called after a change that may affect the plugin catalog (for example a QR bind). */
  onrefresh?: () => void;
} = $props();

let currentConfig = $state<PluginConfigResponse | null>(null);
let configEditRaw = $state<string>('');
let configSaving = $state(false);
let configStatusMsg = $state<string | null>(null);
let configTab = $state<'visual' | 'raw'>('visual');
let qrModalOpen = $state(false);

// Visual form values for the QQ Official adapter.
let qqAppId = $state('');
let qqSecret = $state('');
let qqSecretVisible = $state(false);
let qqIsSandbox = $state(false);
let qqEnableGroupC2C = $state(true);
let qqEnableGuildDm = $state(false);
let qqUseMarkdown = $state(false);
let qqMarkdownTemplateId = $state('');
let qqMarkdownParamsKey = $state('text');

/** Loads the plugin's current configuration and seeds both the raw and visual editors. */
async function loadConfig(id: string) {
  configStatusMsg = null;
  configTab = id === 'org.kanon.adapter.qqofficial' ? 'visual' : 'raw';
  try {
    const res = await api.getPluginConfig(id);
    currentConfig = res;
    configEditRaw = JSON.stringify(res.values, null, 2);
    if (id === 'org.kanon.adapter.qqofficial') {
      syncRawToVisual();
    }
  } catch (e) {
    currentConfig = null;
    configStatusMsg = `Failed to fetch config: ${e instanceof Error ? e.message : String(e)}`;
  }
}

/** Writes the visual QQ Official form back into the raw JSON document. */
function syncVisualToRaw() {
  let parsed: Record<string, unknown> = {};
  try {
    parsed = JSON.parse(configEditRaw);
  } catch {
    parsed = {};
  }
  parsed.appid = qqAppId;
  parsed.secret = qqSecret;
  parsed.is_sandbox = qqIsSandbox;
  parsed.enable_group_c2c = qqEnableGroupC2C;
  parsed.enable_guild_direct_message = qqEnableGuildDm;
  parsed.use_markdown = qqUseMarkdown;
  if (qqMarkdownTemplateId) parsed.markdown_template_id = qqMarkdownTemplateId;
  else delete parsed.markdown_template_id;
  parsed.markdown_params_key = qqMarkdownParamsKey || 'text';

  configEditRaw = JSON.stringify(parsed, null, 2);
}

/** Seeds the visual QQ Official form from the raw JSON document. */
function syncRawToVisual() {
  try {
    const parsed = JSON.parse(configEditRaw);
    if (parsed && typeof parsed === 'object') {
      qqAppId = String(parsed.appid || '');
      qqSecret = String(parsed.secret || '');
      qqIsSandbox = Boolean(parsed.is_sandbox);
      qqEnableGroupC2C = parsed.enable_group_c2c !== false;
      qqEnableGuildDm = Boolean(parsed.enable_guild_direct_message);
      qqUseMarkdown = Boolean(parsed.use_markdown);
      qqMarkdownTemplateId = String(parsed.markdown_template_id || '');
      qqMarkdownParamsKey = String(parsed.markdown_params_key || 'text');
    }
  } catch {
    // ignore
  }
}

/** Saves the document, enforcing the compare-and-swap version the node reported. */
async function saveConfig() {
  if (!pluginId || !currentConfig) return;
  if (pluginId === 'org.kanon.adapter.qqofficial' && configTab === 'visual') {
    syncVisualToRaw();
  }
  configSaving = true;
  configStatusMsg = null;
  try {
    const parsed = JSON.parse(configEditRaw);
    const res = await api.updatePluginConfig(pluginId, parsed, currentConfig.version);
    currentConfig.version = res.version;
    currentConfig.values = res.values;
    configStatusMsg = 'Configuration saved successfully (CAS enforced).';
  } catch (e) {
    configStatusMsg = `Save failed: ${e instanceof Error ? e.message : String(e)}`;
  } finally {
    configSaving = false;
  }
}

/** Adopts credentials obtained through the QR flow and mirrors them into the visual form. */
function handleBound(credentials: { appid: string | null; secret: string | null }) {
  if (credentials.appid) qqAppId = credentials.appid;
  if (credentials.secret) qqSecret = credentials.secret;
  syncVisualToRaw();
  onrefresh?.();
}

// Reload whenever the drawer is pointed at a different plugin.
$effect(() => {
  if (pluginId) {
    void loadConfig(pluginId);
  }
});
</script>

<!-- Plugin configuration drawer -->
{#if pluginId}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="fixed inset-0 bg-black/40 backdrop-blur-xs z-50 flex items-center justify-center p-4"
    onclick={onclose}
    role="button"
    tabindex="-1"
  >
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div
      class="w-full max-w-2xl bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl shadow-2xl p-6 space-y-4"
      onclick={(e) => e.stopPropagation()}
      role="dialog"
      tabindex="-1"
    >
      <div class="flex items-center justify-between border-b border-zinc-200 dark:border-zinc-800 pb-3">
        <div>
          <h3 class="text-base font-semibold text-zinc-900 dark:text-zinc-100">Plugin Config: {pluginId}</h3>
          <span class="text-xs font-mono text-zinc-500">{t('plugins.cas_version')}: {currentConfig?.version ?? 0}</span>
        </div>
        <button
          onclick={onclose}
          class="text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200 text-xs sm:text-sm font-mono cursor-pointer"
        >
          {t('common.close')}
        </button>
      </div>

      {#if configStatusMsg}
        <div class="p-3 text-xs sm:text-sm rounded-lg bg-zinc-100 dark:bg-zinc-800 font-mono text-zinc-700 dark:text-zinc-300">
          {configStatusMsg}
        </div>
      {/if}

      {#if pluginId === 'org.kanon.adapter.qqofficial'}
        <!-- Tabs for QQ Official: Visual Form vs Raw JSON -->
        <div class="flex items-center gap-2 border-b border-zinc-200 dark:border-zinc-800 pb-2">
          <button
            onclick={() => { configTab = 'visual'; syncRawToVisual(); }}
            class="px-3 py-1.5 text-xs font-medium rounded-lg transition cursor-pointer {configTab === 'visual' ? 'bg-indigo-600 text-white' : 'text-zinc-600 dark:text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800'}"
          >
            {t('adapters.qq_visual_config')}
          </button>
          <button
            onclick={() => { configTab = 'raw'; syncVisualToRaw(); }}
            class="px-3 py-1.5 text-xs font-medium rounded-lg transition cursor-pointer {configTab === 'raw' ? 'bg-indigo-600 text-white' : 'text-zinc-600 dark:text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800'}"
          >
            原始 JSON
          </button>
        </div>
      {/if}

      {#if pluginId === 'org.kanon.adapter.qqofficial' && configTab === 'visual'}
        <!-- QR Quick Bind Banner -->
        <div class="p-3.5 rounded-xl bg-gradient-to-r from-emerald-500/10 via-teal-500/10 to-transparent border border-emerald-500/20 flex flex-col sm:flex-row sm:items-center justify-between gap-3">
          <div class="space-y-0.5">
            <div class="font-semibold text-xs sm:text-sm text-emerald-800 dark:text-emerald-300 flex items-center gap-1.5">
              <QrCode class="w-4 h-4 text-emerald-600" />
              <span>{t('adapters.qq_qr_btn')}</span>
            </div>
            <p class="text-xs text-zinc-500 dark:text-zinc-400">使用手机 QQ 扫码一键写入并激活机器人凭据，免去手动查找</p>
          </div>
          <button
            onclick={() => (qrModalOpen = true)}
            class="px-3 py-1.5 bg-emerald-600 hover:bg-emerald-500 text-white rounded-lg text-xs font-medium transition cursor-pointer shadow-2xs flex items-center gap-1.5 shrink-0 self-start sm:self-auto"
          >
            <QrCode class="w-3.5 h-3.5" />
            <span>立即扫码绑定</span>
          </button>
        </div>

        <!-- Visual Form Fields -->
        <div class="space-y-3.5 text-xs sm:text-sm">
          <div class="grid grid-cols-1 sm:grid-cols-2 gap-3">
            <div>
              <!-- svelte-ignore a11y_label_has_associated_control -->
              <label class="block text-xs font-medium text-zinc-600 dark:text-zinc-400 mb-1">{t('adapters.qq_appid')}</label>
              <input
                type="text"
                bind:value={qqAppId}
                oninput={syncVisualToRaw}
                placeholder="例如: 102345678"
                class="w-full px-3 py-2 rounded-lg bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 font-mono text-xs sm:text-sm focus:outline-hidden"
              />
            </div>
            <div>
              <!-- svelte-ignore a11y_label_has_associated_control -->
              <label class="block text-xs font-medium text-zinc-600 dark:text-zinc-400 mb-1">{t('adapters.qq_secret')}</label>
              <div class="relative">
                <input
                  type={qqSecretVisible ? 'text' : 'password'}
                  bind:value={qqSecret}
                  oninput={syncVisualToRaw}
                  placeholder="AppSecret 密钥"
                  class="w-full px-3 py-2 pr-9 rounded-lg bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 font-mono text-xs sm:text-sm focus:outline-hidden"
                />
                <button
                  type="button"
                  onclick={() => (qqSecretVisible = !qqSecretVisible)}
                  class="absolute right-2.5 top-1/2 -translate-y-1/2 text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200 cursor-pointer"
                >
                  {#if qqSecretVisible}
                    <EyeOff class="w-4 h-4" />
                  {:else}
                    <Eye class="w-4 h-4" />
                  {/if}
                </button>
              </div>
            </div>
          </div>

          <div class="grid grid-cols-1 sm:grid-cols-2 gap-3 pt-1">
            <div>
              <!-- svelte-ignore a11y_label_has_associated_control -->
              <label class="block text-xs font-medium text-zinc-600 dark:text-zinc-400 mb-1">{t('adapters.qq_md_template')}</label>
              <input
                type="text"
                bind:value={qqMarkdownTemplateId}
                oninput={syncVisualToRaw}
                placeholder="可选自定义模板 ID"
                class="w-full px-3 py-2 rounded-lg bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 font-mono text-xs sm:text-sm focus:outline-hidden"
              />
            </div>
            <div>
              <!-- svelte-ignore a11y_label_has_associated_control -->
              <label class="block text-xs font-medium text-zinc-600 dark:text-zinc-400 mb-1">{t('adapters.qq_md_param')}</label>
              <input
                type="text"
                bind:value={qqMarkdownParamsKey}
                oninput={syncVisualToRaw}
                placeholder="text"
                class="w-full px-3 py-2 rounded-lg bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 font-mono text-xs sm:text-sm focus:outline-hidden"
              />
            </div>
          </div>

          <!-- Feature Toggles -->
          <div class="grid grid-cols-1 sm:grid-cols-2 gap-2.5 pt-1">
            <label class="flex items-center gap-2.5 p-2.5 rounded-lg border border-zinc-200 dark:border-zinc-800 bg-zinc-50/50 dark:bg-zinc-950/40 cursor-pointer">
              <input
                type="checkbox"
                bind:checked={qqEnableGroupC2C}
                onchange={syncVisualToRaw}
                class="rounded border-zinc-300 text-indigo-600 focus:ring-indigo-500 w-4 h-4"
              />
              <span class="text-xs text-zinc-700 dark:text-zinc-300">{t('adapters.qq_group_c2c')}</span>
            </label>
            <label class="flex items-center gap-2.5 p-2.5 rounded-lg border border-zinc-200 dark:border-zinc-800 bg-zinc-50/50 dark:bg-zinc-950/40 cursor-pointer">
              <input
                type="checkbox"
                bind:checked={qqEnableGuildDm}
                onchange={syncVisualToRaw}
                class="rounded border-zinc-300 text-indigo-600 focus:ring-indigo-500 w-4 h-4"
              />
              <span class="text-xs text-zinc-700 dark:text-zinc-300">{t('adapters.qq_guild_dm')}</span>
            </label>
            <label class="flex items-center gap-2.5 p-2.5 rounded-lg border border-zinc-200 dark:border-zinc-800 bg-zinc-50/50 dark:bg-zinc-950/40 cursor-pointer">
              <input
                type="checkbox"
                bind:checked={qqUseMarkdown}
                onchange={syncVisualToRaw}
                class="rounded border-zinc-300 text-indigo-600 focus:ring-indigo-500 w-4 h-4"
              />
              <span class="text-xs text-zinc-700 dark:text-zinc-300">{t('adapters.qq_use_markdown')}</span>
            </label>
            <label class="flex items-center gap-2.5 p-2.5 rounded-lg border border-zinc-200 dark:border-zinc-800 bg-zinc-50/50 dark:bg-zinc-950/40 cursor-pointer">
              <input
                type="checkbox"
                bind:checked={qqIsSandbox}
                onchange={syncVisualToRaw}
                class="rounded border-zinc-300 text-indigo-600 focus:ring-indigo-500 w-4 h-4"
              />
              <span class="text-xs text-zinc-700 dark:text-zinc-300">{t('adapters.qq_sandbox')}</span>
            </label>
          </div>
        </div>
      {:else}
        <div>
          <!-- svelte-ignore a11y_label_has_associated_control -->
          <label class="block text-xs sm:text-sm font-medium text-zinc-600 dark:text-zinc-400 mb-1.5">Configuration (JSON)</label>
          <textarea
            bind:value={configEditRaw}
            oninput={() => { if (pluginId === 'org.kanon.adapter.qqofficial') syncRawToVisual(); }}
            rows={10}
            class="w-full p-3 bg-zinc-950 font-mono text-xs sm:text-sm text-zinc-200 border border-zinc-800 rounded-lg focus:outline-hidden"
          ></textarea>
        </div>
      {/if}

      <div class="flex items-center justify-end gap-2 pt-2">
        <button
          onclick={onclose}
          class="px-3.5 py-2 text-xs sm:text-sm text-zinc-600 dark:text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded-lg transition cursor-pointer"
        >
          {t('common.cancel')}
        </button>
        <button
          onclick={saveConfig}
          disabled={configSaving}
          class="px-4 py-2 text-xs sm:text-sm bg-indigo-600 hover:bg-indigo-500 text-white rounded-lg font-medium transition cursor-pointer flex items-center gap-1.5 disabled:opacity-50"
        >
          <Save class="w-4 h-4" />
          <span>{configSaving ? 'Enforcing CAS...' : t('common.save')}</span>
        </button>
      </div>
    </div>
  </div>
{/if}
<QqOfficialQrModal
  open={qrModalOpen}
  onclose={() => (qrModalOpen = false)}
  onbound={handleBound}
/>
