/**
 * The operator's saved configuration must reach a TypeScript plugin, at startup and on every
 * reload, and a reload the plugin rejects must leave it on the configuration it accepted last.
 *
 * The real host process is spawned in a scratch working directory, because it reads
 * `./data/plugins/<id>/config.json` relative to its working directory.
 */

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";

import * as grpc from "@grpc/grpc-js";

import { loadKanonProto, toProtoStruct } from "../src/sdk/index.js";

const PLUGIN_ID = "org.kanon.test.config";

// The plugin reports the configuration it currently sees through its metadata description.
const PLUGIN_SOURCE = `
export default class {
  meta() {
    return {
      id: "${PLUGIN_ID}", name: "Config", version: "0.1.0", author: "", commands: [], tools: [],
      description: JSON.stringify(this.ctx ? this.ctx.config : null),
    };
  }
  async onLoad(ctx) { this.ctx = ctx; }
  async onConfigReload(config) { if (config.reject) throw new Error("rejected"); }
  async onUnload() {}
}
`;

/** Resolves after `ms`. */
function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Promisifies one unary call on a dynamically loaded gRPC client. */
function unary(client: any, method: string, request: any): Promise<any> {
  return new Promise((resolve, reject) =>
    client[method](request, { deadline: Date.now() + 3000 },
      (err: any, res: any) => (err ? reject(err) : resolve(res))),
  );
}

test("stored and reloaded configuration reach the plugin", async () => {
  const workDir = fs.mkdtempSync(path.join(os.tmpdir(), "kanon-ts-config-"));
  const dataDir = path.join(workDir, "data", "plugins", PLUGIN_ID);
  fs.mkdirSync(dataDir, { recursive: true });
  fs.writeFileSync(path.join(dataDir, "config.json"), JSON.stringify({ city: "Paris" }));
  const pluginPath = path.join(workDir, "plugin.mjs");
  fs.writeFileSync(pluginPath, PLUGIN_SOURCE);
  const socketPath = path.join(workDir, "host.sock");

  const host = spawn(
    process.execPath,
    [path.resolve(__dirname, "../src/host/index.js"), "--plugin", pluginPath],
    { cwd: workDir, env: { ...process.env, KANON_HOST_SOCK: socketPath }, stdio: "ignore" },
  );

  try {
    const kanonV1 = (loadKanonProto() as any).kanon.plugin.v1;
    const client = new kanonV1.PluginHostService(
      `unix://${socketPath}`,
      grpc.credentials.createInsecure(),
    );
    const description = async (): Promise<string> =>
      (await unary(client, "GetPluginMeta", {})).plugins[0].description;

    let seen: string | undefined;
    for (let attempt = 0; attempt < 100 && seen === undefined; attempt++) {
      try {
        seen = await description();
      } catch {
        await sleep(50);
      }
    }
    assert.equal(seen, JSON.stringify({ city: "Paris" }));

    const accepted = await unary(client, "ReloadPluginConfig", {
      plugin_id: PLUGIN_ID,
      config: toProtoStruct({ city: "Rome" }),
      version: 1,
    });
    assert.equal(accepted.success, true, accepted.error_message);
    assert.equal(await description(), JSON.stringify({ city: "Rome" }));

    const rejected = await unary(client, "ReloadPluginConfig", {
      plugin_id: PLUGIN_ID,
      config: toProtoStruct({ reject: true }),
      version: 2,
    });
    assert.equal(rejected.success, false);
    assert.equal(await description(), JSON.stringify({ city: "Rome" }));
    // Invalid wire numbers must reject the reload without killing the host or advancing its version.
    const invalid = await unary(client, "ReloadPluginConfig", {
      plugin_id: PLUGIN_ID,
      config: { fields: { bad: { numberValue: Number.NaN } } },
      version: 2,
    });
    assert.equal(invalid.success, false);
    assert.match(invalid.error_message, /finite/);
    assert.equal(await description(), JSON.stringify({ city: "Rome" }));
    const retried = await unary(client, "ReloadPluginConfig", {
      plugin_id: PLUGIN_ID,
      config: toProtoStruct({ city: "Tokyo" }),
      version: 2,
    });
    assert.equal(retried.success, true, retried.error_message);
    assert.equal(await description(), JSON.stringify({ city: "Tokyo" }));
    client.close();
  } finally {
    host.kill("SIGKILL");
    fs.rmSync(workDir, { recursive: true, force: true });
  }
});
