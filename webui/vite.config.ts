import { svelte } from '@sveltejs/vite-plugin-svelte';
import tailwindcss from '@tailwindcss/vite';
import type { Plugin } from 'vite';
import { defineConfig } from 'vite';

/**
 * Points `@material/web/labs/gb` stylesheet imports at the CSSResult files the package also ships.
 *
 * The Material 3 Expressive components import their styles as CSS module scripts:
 *
 *     import switchStyles from './switch.css' with { type: 'css' };
 *
 * Vite and rolldown cannot bundle that form yet — the import attribute makes them look for a JS
 * module named `switch.css`, so the build fails with "default is not exported". Every one of those
 * imports has a sibling `<name>.cssresult.js` that default-exports the very same CSSStyleSheet,
 * which is what Google's own internal build uses (the shipped file even keeps that line, commented
 * out, right below the active one). Rewriting the specifier is therefore not a workaround on our
 * side but a switch between two variants the package already publishes.
 *
 * Only the expressive component tree is rewritten: the package's plain stylesheet imports (its
 * token sheets, which we import ourselves) and every other `.css` import are left untouched.
 *
 * This is why `@material/web` is excluded from dependency pre-bundling below: the optimiser strips
 * the `with { type: 'css' }` attribute and rewrites the specifier to an absolute `/node_modules/…`
 * path, so neither half of the match above holds any more and dev served the raw CSS import — which
 * throws "doesn't provide an export named: 'default'" and leaves the whole page blank. Serving the
 * package as source keeps dev and the production build on the same code path.
 */
function gbCssResult(): Plugin {
  return {
    name: 'kanon:gb-cssresult',
    enforce: 'pre',
    transform(code, id) {
      if (!id.includes('/@material/web/labs/gb/components/')) return null;
      if (!code.includes("with { type: 'css' }")) return null;
      return code.replace(
        /from\s+('[^']+\.css')\s+with\s*\{\s*type:\s*'css'\s*\}/g,
        (_match, specifier: string) =>
          `from ${specifier.replace(/\.css'$/, ".cssresult.js'")}`,
      );
    },
  };
}

// https://vite.dev/config/
export default defineConfig({
  plugins: [gbCssResult(), svelte(), tailwindcss()],
  optimizeDeps: {
    // See `gbCssResult` above: pre-bundling destroys the import form that shim rewrites, which left
    // `bun run dev` with a blank page while `vite build` worked.
    exclude: ['@material/web'],
  },
  server: {
    host: '0.0.0.0',
    port: 5173,
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:8080',
        changeOrigin: true,
      },
      '/ws': {
        target: 'http://127.0.0.1:8080',
        ws: true,
        changeOrigin: true,
      },
    },
  },
});
