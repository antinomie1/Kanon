/** Real loopback requests cover the Windows metadata authentication contract. */
import assert from "node:assert/strict";
import test from "node:test";
import * as grpc from "@grpc/grpc-js";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { once } from "node:events";
import { clientAuth, serverAuth, loopbackEndpoint } from "../src/sdk/ipc.js";

test("loopback RPC rejects missing/wrong credentials and accepts matching credentials", async () => {
  const token = "ab".repeat(32);
  const server = new grpc.Server({ interceptors: [serverAuth(token)] });
  const service: grpc.ServiceDefinition = { ping: {
    path: "/test/Ping", requestStream: false, responseStream: false,
    requestSerialize: (v: Buffer) => v, requestDeserialize: (v: Buffer) => v,
    responseSerialize: (v: Buffer) => v, responseDeserialize: (v: Buffer) => v,
  } };
  server.addService(service, { ping: (_call: unknown, callback: Function) => callback(null, Buffer.from("ok")) });
  const port = await new Promise<number>((resolve, reject) => server.bindAsync("127.0.0.1:0", grpc.ServerCredentials.createInsecure(), (error, port) => error ? reject(error) : resolve(port)));
  const Client = grpc.makeGenericClientConstructor(service, "Test");
  try {
    for (const credential of [undefined, "cd".repeat(32), token]) {
      const client = new Client(`127.0.0.1:${port}`, grpc.credentials.createInsecure(), { interceptors: credential ? [clientAuth(credential)] : [] });
      try {
        const response = new Promise<Buffer>((resolve, reject) => client.ping(Buffer.alloc(0), (error: grpc.ServiceError | null, value: Buffer) => error ? reject(error) : resolve(value)));
        if (credential === token) assert.equal((await response).toString(), "ok");
        else await assert.rejects(response, { code: grpc.status.UNAUTHENTICATED });
      } finally { client.close(); }
    }
  } finally { server.forceShutdown(); }
});

test("endpoint file must contain a literal loopback address", () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "kanon-ipc-"));
  const file = path.join(directory, "core.sock");
  try {
    for (const address of ["192.0.2.1:8000", "localhost:8000", "127.0.0.1:0"]) {
      fs.writeFileSync(file, address);
      assert.throws(() => loopbackEndpoint(file));
    }
    fs.writeFileSync(file, "127.0.0.1:8000");
    assert.equal(loopbackEndpoint(file), "127.0.0.1:8000");
  } finally { fs.rmSync(directory, { recursive: true }); }
});

test("endpoint preparation preserves live/files/symlinks and recovers a crashed host", { skip: process.platform === "win32" }, async () => {
  const { prepareEndpoint } = await import("../src/sdk/ipc.js");
  const { spawn } = await import("node:child_process");
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "kanon-endpoint-"));
  const file = path.join(directory, "host.sock");
  const child = spawn(process.execPath, ["-e", "require('node:net').createServer().listen(process.argv[1], () => process.stdout.write('ready'))", file], { stdio: ["ignore", "pipe", "pipe"] });
  try {
    await once(child.stdout!, "data");
    await assert.rejects(prepareEndpoint(file), /active/);
    const exited = once(child, "exit");
    child.kill("SIGKILL");
    await exited;
    await prepareEndpoint(file);
    assert.equal(fs.existsSync(file), false);
    fs.writeFileSync(file, "keep");
    await assert.rejects(prepareEndpoint(file), /invalid file type/);
    assert.equal(fs.readFileSync(file, "utf8"), "keep");
    fs.unlinkSync(file);
    fs.symlinkSync(path.join(directory, "missing"), file);
    await assert.rejects(prepareEndpoint(file), /invalid file type/);
    assert.equal(fs.lstatSync(file).isSymbolicLink(), true);
  } finally {
    child.kill("SIGKILL");
    fs.rmSync(directory, { recursive: true });
  }
});
