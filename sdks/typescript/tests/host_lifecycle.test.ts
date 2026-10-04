/** A stuck registration, unload hook or active RPC must not leave a ghost platform host. */
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import test from "node:test";
import * as grpc from "@grpc/grpc-js";
import { loadKanonProto } from "../src/sdk/index.js";

/** Waits for a fixture marker with a finite setup budget. */
async function waitForFile(file: string): Promise<void> {
  for (let attempt = 0; attempt < 200; attempt++) {
    if (fs.existsSync(file)) return;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  assert.fail(`fixture did not create ${path.basename(file)}`);
}

for (const hangUnload of [true, false]) {
  test(`shutdown bounds ${hangUnload ? "plugin teardown" : "RPC draining"} during Core registration`, {
    skip: process.platform === "win32",
    timeout: 12000,
  }, async () => {
    const workDir = fs.mkdtempSync(path.join(os.tmpdir(), "kanon-host-stop-"));
    const core = new grpc.Server();
    const kanonV1 = (loadKanonProto() as any).kanon.plugin.v1;
    core.addService(kanonV1.BotApiService.service, {
      RegisterHost: () => { fs.writeFileSync(path.join(workDir, "registered"), ""); },
    });
    const port = await new Promise<number>((resolve, reject) => core.bindAsync("127.0.0.1:0", grpc.ServerCredentials.createInsecure(), (error, port) => error ? reject(error) : resolve(port)));
    const pluginPath = path.join(workDir, "plugin.mjs");
    const socketPath = path.join(workDir, "host.sock");
    fs.writeFileSync(pluginPath, `
      import * as fs from "node:fs";
      export default class {
        meta() { return { id: "test.stop", name: "Stop", version: "1" }; }
        async onLoad() {}
        async onCallTool() {
          fs.writeFileSync("tool-started", "");
          await new Promise(() => {});
        }
        async onUnload() {
          fs.writeFileSync("unloaded", "");
          ${hangUnload ? "await new Promise(() => {});" : ""}
        }
      }
    `);
    const host = spawn(process.execPath, [path.resolve(__dirname, "../src/host/index.js"), "--plugin", pluginPath], {
      cwd: workDir,
      env: { ...process.env, KANON_CORE_SOCK: `127.0.0.1:${port}`, KANON_HOST_SOCK: socketPath, KANON_IPC_TOKEN: "" },
      stdio: "ignore",
    });
    const exited = once(host, "exit");
    const deadline = setTimeout(() => host.kill("SIGKILL"), 10000);
    const client = new kanonV1.MessagePipelineService(`unix:${socketPath}`, grpc.credentials.createInsecure());
    try {
      // The fake Core accepts the connection but never answers RegisterHost. Shutdown handlers
      // must already be installed while the host is awaiting that registration response.
      await waitForFile(path.join(workDir, "registered"));
      const tool = new Promise<grpc.ServiceError | null>((resolve) => {
        client.OnCallTool({ call_id: "pending", tool_name: "pending" }, (error: grpc.ServiceError | null) => resolve(error));
      });
      await waitForFile(path.join(workDir, "tool-started"));
      host.kill("SIGTERM");
      await waitForFile(path.join(workDir, "unloaded"));
      const [code, signal] = await exited;
      assert.equal(signal, null, "the host must finish its shutdown handler");
      assert.equal(code, 0);
      assert.equal(fs.existsSync(socketPath), false, "shutdown must release the owned endpoint");
      assert.ok(await tool, "the pending RPC must be terminated with the server");
    } finally {
      clearTimeout(deadline);
      client.close();
      if (host.exitCode === null && host.signalCode === null) host.kill("SIGKILL");
      await exited;
      core.forceShutdown();
      fs.rmSync(workDir, { recursive: true, force: true });
    }
  });
}
