/** The real host must load the manifest's entrypoint and reject malformed declarations. */
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";

/** Starts the host until the selected fixture identifies itself by failing at import time. */
async function loadFixture(manifest: string): Promise<string> {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "kanon-entrypoint-"));
  try {
    const plugin = path.join(directory, "plugins", "fixture");
    const built = path.join(directory, "dist", "plugins", "fixture");
    fs.mkdirSync(plugin, { recursive: true });
    fs.mkdirSync(path.join(built, "src"), { recursive: true });
    fs.writeFileSync(path.join(plugin, "plugin.toml"), manifest);
    fs.writeFileSync(path.join(plugin, "worker #1.mjs"), 'throw new Error("SELECTED_ENTRYPOINT");');
    fs.writeFileSync(path.join(plugin, "wrong.mjs"), 'throw new Error("WRONG_ENTRYPOINT");');
    fs.writeFileSync(path.join(plugin, "index.js"), 'throw new Error("FALLBACK_ENTRYPOINT");');
    fs.writeFileSync(path.join(built, "src", "worker.js"), 'throw new Error("SELECTED_ENTRYPOINT");');
    fs.writeFileSync(path.join(built, "index.js"), 'throw new Error("WRONG_ENTRYPOINT");');
    return await new Promise<string>((resolve, reject) => {
      const host = spawn(
        process.execPath,
        [path.resolve(__dirname, "../src/host/index.js"), "--plugin", path.join(plugin, "plugin.toml")],
        { cwd: directory, stdio: ["ignore", "ignore", "pipe"] },
      );
      let stderr = "";
      host.stderr.on("data", (chunk: Buffer) => { stderr += chunk.toString(); });
      const timeout = setTimeout(() => {
        host.kill("SIGKILL");
        reject(new Error("host did not finish loading its entrypoint"));
      }, 5000);
      host.on("error", (error) => {
        clearTimeout(timeout);
        reject(error);
      });
      host.on("close", (code) => {
        clearTimeout(timeout);
        if (code !== 1) reject(new Error(`unexpected host exit ${code}: ${stderr}`));
        else resolve(stderr);
      });
    });
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
}

test("host respects TOML sections, comments, literal strings and escaped entrypoints", async () => {
  for (const manifest of [
    '# entrypoint = "wrong.mjs"\n[plugin]\nentrypoint = \'worker #1.mjs\'\n',
    '[other]\nentrypoint = "wrong.mjs"\n[plugin]\nentrypoint = "worker #1.mjs"\n',
    '[plugin]\nentrypoint = "worker \\u00231.mjs"\n',
    '[plugin]\nentrypoint = "src/worker.ts"\n',
  ]) {
    const stderr = await loadFixture(manifest);
    assert.match(stderr, /SELECTED_ENTRYPOINT/);
    assert.doesNotMatch(stderr, /WRONG_ENTRYPOINT|FALLBACK_ENTRYPOINT/);
  }
});

test("host rejects missing, invalid and malformed manifest entrypoints", async () => {
  for (const manifest of [
    '[other]\nentrypoint = "wrong.mjs"\n',
    '[plugin]\nentrypoint = 123\n',
    '[plugin]\nentrypoint = ""\n',
    '[plugin]\nentrypoint = "worker #1.mjs"\ninvalid = [\n',
  ]) {
    const stderr = await loadFixture(manifest);
    assert.match(stderr, /nonempty \[plugin\]\.entrypoint|TomlError/);
    assert.doesNotMatch(stderr, /SELECTED_ENTRYPOINT|WRONG_ENTRYPOINT|FALLBACK_ENTRYPOINT/);
  }
});
