<script lang="ts">
import type { CommandPolicyDraft } from '../../stores/commandPolicy.svelte';
import { t } from '../../stores/i18n.svelte';
import type { CommandAccess } from '../../types';
import SupportBadge from './SupportBadge.svelte';

/**
 * Editor for one command policy: administrators, whether group roles count, and the access table.
 *
 * Shared by the node-wide settings and the per-instance override so both edit exactly the same
 * fields; the owner decides when the draft is saved.
 */
let { draft = $bindable() }: { draft: CommandPolicyDraft } = $props();

let newCommand = $state('');

/** Access levels in the order the selector lists them. */
const accessLevels: { value: CommandAccess; labelKey: string }[] = [
  { value: 'everyone', labelKey: 'commands.level_everyone' },
  { value: 'admins_in_groups', labelKey: 'commands.level_admins_in_groups' },
  { value: 'admins', labelKey: 'commands.level_admins' },
];

/** Adds a command row, restricted to administrators by default. */
function addCommand() {
  const command = newCommand.trim().replace(/^\//, '').toLowerCase();
  if (!command || draft.rows.some((row) => row.command === command)) return;
  draft.rows = [...draft.rows, { command, access: 'admins' }];
  newCommand = '';
}
</script>

<div class="grid grid-cols-1 md:grid-cols-2 gap-6">
  <div class="space-y-3">
    <label class="block">
      <span class="block text-sm font-medium text-zinc-800 dark:text-zinc-200">{t('commands.admins')}</span>
      <span class="block text-xs text-zinc-500 mt-0.5 mb-2">{t('commands.admins_hint')}</span>
      <textarea
        bind:value={draft.admins}
        rows="4"
        placeholder="onebot:12345&#10;qqofficial:5361A5D2..."
        class="w-full px-3 py-2 rounded-lg bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 font-mono text-xs focus:outline-hidden"
      ></textarea>
    </label>
    <label class="flex items-start justify-between gap-4 cursor-pointer select-none">
      <span>
        <span class="block text-sm font-medium text-zinc-800 dark:text-zinc-200">{t('commands.group_admins')}</span>
        <span class="block text-xs text-zinc-500 mt-0.5">{t('commands.group_admins_hint')}</span>
        <SupportBadge capabilities={['sender_role']} />
      </span>
      <input
        type="checkbox"
        bind:checked={draft.groupAdminsAreAdmins}
        class="mt-1 rounded text-indigo-600 focus:ring-0 w-4 h-4 shrink-0"
      />
    </label>
  </div>

  <div class="space-y-2">
    <span class="block text-sm font-medium text-zinc-800 dark:text-zinc-200">{t('commands.access')}</span>
    <span class="block text-xs text-zinc-500">{t('commands.access_hint')}</span>
    {#each draft.rows as row, index (row.command)}
      <div class="flex items-center gap-2">
        <span class="w-28 font-mono text-sm text-zinc-700 dark:text-zinc-300">/{row.command}</span>
        <select
          bind:value={draft.rows[index].access}
          class="flex-1 px-2 py-1.5 text-xs bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-md cursor-pointer"
        >
          {#each accessLevels as level (level.value)}
            <option value={level.value}>{t(level.labelKey)}</option>
          {/each}
        </select>
        <button
          type="button"
          onclick={() => (draft.rows = draft.rows.filter((_, i) => i !== index))}
          class="px-2 py-1 text-xs text-zinc-500 hover:text-rose-600 cursor-pointer"
          title={t('commands.remove')}
        >
          ✕
        </button>
      </div>
    {/each}
    <div class="flex items-center gap-2 pt-1">
      <input
        bind:value={newCommand}
        placeholder={t('commands.add_placeholder')}
        onkeydown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            addCommand();
          }
        }}
        class="flex-1 px-2 py-1.5 text-xs font-mono bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-md focus:outline-hidden"
      />
      <button
        type="button"
        onclick={addCommand}
        class="px-3 py-1.5 text-xs border border-zinc-200 dark:border-zinc-700 rounded-md hover:bg-zinc-100 dark:hover:bg-zinc-800 cursor-pointer"
      >
        {t('commands.add')}
      </button>
    </div>
  </div>
</div>
