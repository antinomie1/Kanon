<script lang="ts">
import { Eye, EyeOff } from 'lucide-svelte';
import { t } from '../../stores/i18n.svelte';

/**
 * Password-style input with a show/hide toggle, for tokens and secrets the node never returns.
 *
 * The browser is told not to autofill it: these are credentials for other services, and a saved
 * console login filled in here would be sent to them.
 */
let {
  value = $bindable(''),
  id,
  placeholder,
}: {
  value?: string;
  id?: string;
  placeholder?: string;
} = $props();

let shown = $state(false);
</script>

<div class="relative">
  <input
    {id}
    type={shown ? 'text' : 'password'}
    class="input mono pr-12"
    autocomplete="new-password"
    spellcheck="false"
    {placeholder}
    bind:value
  />
  <button
    type="button"
    class="btn btn-quiet btn-icon btn-xs absolute top-1/2 right-2 -translate-y-1/2"
    aria-label={shown ? t('platforms.hide_secret') : t('platforms.show_secret')}
    aria-pressed={shown}
    onclick={() => (shown = !shown)}
  >
    {#if shown}
      <EyeOff size={16} strokeWidth={2} />
    {:else}
      <Eye size={16} strokeWidth={2} />
    {/if}
  </button>
</div>
