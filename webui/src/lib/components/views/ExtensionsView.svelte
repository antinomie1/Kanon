<script lang="ts">
import { t } from '../../stores/i18n.svelte';
import { router } from '../../stores/router.svelte';
import McpTab from '../extensions/McpTab.svelte';
import PluginsTab from '../extensions/PluginsTab.svelte';
import SkillsTab from '../extensions/SkillsTab.svelte';
import ToolsTab from '../extensions/ToolsTab.svelte';
import PageHead from '../ui/PageHead.svelte';
import Seg from '../ui/Seg.svelte';

/**
 * Extensions: everything that adds abilities to the model. Plugins, MCP servers and skills are
 * three providers, and the tool list shows what they add up to, so they share one page with a tab
 * each. The tab is part of the address (`#/extensions/mcp`) so a reload keeps it.
 */

const TABS = ['plugins', 'tools', 'mcp', 'skills'] as const;
type Tab = (typeof TABS)[number];

const tab = $derived<Tab>(
  (TABS as readonly string[]).includes(router.param ?? '')
    ? (router.param as Tab)
    : 'plugins',
);
</script>

<PageHead title={t('nav.extensions')}>
  {#snippet sub()}
    <span>{t('extensions.sub')}</span>
  {/snippet}
</PageHead>

<div class="scroll-thin -mt-1 overflow-x-auto px-1">
  <Seg
    label={t('nav.extensions')}
    value={tab}
    onchange={(next: Tab) => router.replaceParam(next === 'plugins' ? null : next)}
    options={[
      { value: 'plugins', label: t('extensions.tab_plugins') },
      { value: 'tools', label: t('extensions.tab_tools') },
      { value: 'mcp', label: t('extensions.tab_mcp') },
      { value: 'skills', label: t('extensions.tab_skills') },
    ]}
  />
</div>

{#if tab === 'tools'}
  <ToolsTab />
{:else if tab === 'mcp'}
  <McpTab />
{:else if tab === 'skills'}
  <SkillsTab />
{:else}
  <PluginsTab />
{/if}
