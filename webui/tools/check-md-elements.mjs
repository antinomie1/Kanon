/**
 * Checks that every Material custom element the console renders is actually registered.
 *
 * A missing import in `src/lib/md.ts` is invisible to the type checker and to a component-level
 * sandbox that registers elements itself: the tag still renders, but as an unstyled unknown element
 * with no shadow root — which is how `md-outlined-segmented-button-set` shipped at 0px wide for a
 * round. This walks the templates for `md-*` tags and compares them with the tags the modules
 * imported by `md.ts` declare, either through `customElements.define('…')` or Lit's
 * `customElement('…')` decorator.
 *
 * Run from `webui/`:  bun tools/check-md-elements.mjs
 */
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join } from 'node:path';

const SRC = 'src';
const REGISTRY = 'src/lib/md.ts';
const PACKAGE = 'node_modules/@material/web';

/** Every `md-*` tag the templates render. */
function usedTags() {
  const tags = new Set();
  const walk = (dir) => {
    for (const name of readdirSync(dir)) {
      const path = join(dir, name);
      if (statSync(path).isDirectory()) walk(path);
      else if (path.endsWith('.svelte')) {
        for (const m of readFileSync(path, 'utf8').matchAll(/<(md-[a-z0-9-]+)[\s>]/g)) tags.add(m[1]);
      }
    }
  };
  walk(SRC);
  return tags;
}

/** Every `md-*` tag the modules imported by the registry declare. */
function registeredTags() {
  const registry = readFileSync(REGISTRY, 'utf8');
  const modules = [...registry.matchAll(/import '(@material\/web\/[^']+)'/g)].map((m) => m[1]);
  const tags = new Set();
  for (const module of modules) {
    const contents = readFileSync(join(PACKAGE, module.replace('@material/web/', '')), 'utf8');
    for (const m of contents.matchAll(/customElements\.define\('(md-[a-z0-9-]+)'/g)) tags.add(m[1]);
    for (const m of contents.matchAll(/customElement\('(md-[a-z0-9-]+)'/g)) tags.add(m[1]);
  }
  return { tags, modules: modules.length };
}

const used = usedTags();
const { tags: registered, modules } = registeredTags();
const missing = [...used].filter((tag) => !registered.has(tag)).sort();

console.log(
  `md elements: ${used.size} used, ${registered.size} registered by ${modules} imports in ${REGISTRY}`,
);
if (missing.length) {
  console.error(
    `\nNot registered: ${missing.join(', ')}\n` +
      `Add the module that defines each one to ${REGISTRY}; a custom element without its import\n` +
      `renders as an unstyled unknown element with no shadow root.`,
  );
  process.exit(1);
}
console.log('every rendered md-* element is registered');
