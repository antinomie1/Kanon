<script lang="ts" module>
/**
 * Clip path of Material 3 Expressive's nine-lobed "cookie" shape, the one decorative shape the
 * console uses. Traced once per page load from r(θ) = 50% − depth·(1 − cos 9θ)/2, rotated so a lobe
 * points straight up; twelve points per lobe keep the scallops smooth at the sizes drawn here.
 */
const COOKIE = (() => {
  const lobes = 9;
  const depth = 7;
  const steps = lobes * 12;
  const points: string[] = [];
  for (let i = 0; i < steps; i++) {
    const angle = (i / steps) * 2 * Math.PI;
    const radius = 50 - (depth * (1 - Math.cos(lobes * angle))) / 2;
    const x = 50 + radius * Math.sin(angle);
    const y = 50 - radius * Math.cos(angle);
    points.push(`${x.toFixed(2)}% ${y.toFixed(2)}%`);
  }
  return `polygon(${points.join(', ')})`;
})();
</script>

<script lang="ts">
import type { Snippet } from 'svelte';
import type { IconComponent } from '../../types';

/**
 * An empty list or panel: what is missing, and the action that fills it. The icon sits in the
 * cookie shape in the tertiary container colour: Material 3 Expressive keeps tertiary for
 * contrasting accents, and an empty page is where the console can afford one.
 */
let {
  icon: Icon,
  title,
  text,
  action,
  compact = false,
}: {
  icon?: IconComponent;
  title: string;
  text?: string;
  action?: Snippet;
  compact?: boolean;
} = $props();
</script>


<div class="flex flex-col items-center text-center {compact ? 'gap-2 px-4 py-8' : 'gap-3 px-6 py-14'}">
  {#if Icon}
    <div
      class="grid place-items-center bg-tertiary-tint text-tertiary-fg {compact ? 'size-12' : 'size-16'}"
      style:clip-path={COOKIE}
    >
      <Icon size={compact ? 22 : 28} strokeWidth={2} />
    </div>
  {/if}
  <div class="max-w-[46ch]">
    <p class="m-0 text-[15.5px] font-semibold">{title}</p>
    {#if text}<p class="m-0 mt-1 hint">{text}</p>{/if}
  </div>
  {#if action}<div class="mt-1 flex flex-wrap justify-center gap-2.5">{@render action()}</div>{/if}
</div>
