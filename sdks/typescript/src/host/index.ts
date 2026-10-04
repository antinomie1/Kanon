/**
 * Kanon Official TypeScript / Node.js Host Process.
 *
 * Spawns a gRPC server hosting TypeScript plugins, providing PluginHostService
 * and MessagePipelineService over an IPC socket (Unix Domain Socket or Windows Loopback TCP).
 */

import { ipcToken, serverAuth, prepareEndpoint } from "../sdk/ipc.js";
import * as fs from "fs";
import * as path from "path";
import { pathToFileURL } from "node:url";
import * as grpc from "@grpc/grpc-js";
import { parse as parseToml } from "@iarna/toml";
import {
  CoreHandle,
  Plugin,
  PluginContext,
  fromProtoStruct,
  loadKanonProto,
  startCoreWatchdog,
} from "../sdk/index.js";

/** Startup budget for the Core endpoint to become reachable before standalone mode. */
const CORE_READY_TIMEOUT_MS = 2000;

/** One budget covers plugin teardown and draining in-flight RPCs before a forced stop. */
const SHUTDOWN_TIMEOUT_MS = 5000;

/**
 * Builds the Core handle that this host process hands to its plugin.
 *
 * Returns `undefined` — standalone mode — when `KANON_CORE_SOCK` is unset or the
 * endpoint never becomes ready within {@link CORE_READY_TIMEOUT_MS}. Handing a plugin
 * a handle to a dead endpoint would turn every later inbound message into a failure,
 * so the host probes reachability up front and logs the standalone decision
 * explicitly instead of fabricating a working context.
 */
async function connectCore(
  coreSockPath: string | undefined,
  identity: { hostId: string; pluginId: string },
): Promise<CoreHandle | undefined> {
  if (!coreSockPath) {
    console.warn(
      "KANON_CORE_SOCK is not set: starting in standalone mode, ctx.core will be undefined",
    );
    return undefined;
  }

  // The identity lets the plugin's KV namespace, agent runs, renders and metadata refreshes name
  // their caller without the plugin passing its own id around.
  const handle = new CoreHandle(coreSockPath, undefined, identity);
  if (!(await handle.waitForReady(CORE_READY_TIMEOUT_MS))) {
    console.warn(
      `Core endpoint '${coreSockPath}' is unreachable: starting in standalone mode, ctx.core will be undefined`,
    );
    // The channel never became ready, so release its resources right away.
    handle.close();
    return undefined;
  }

  console.log(`Connected to Core endpoint ${coreSockPath}`);
  return handle;
}

/** Binds the host gRPC server, resolving once the IPC endpoint accepts connections. */
function bindHostServer(server: grpc.Server, bindAddress: string): Promise<number> {
  return new Promise<number>((resolve, reject) => {
    server.bindAsync(
      bindAddress,
      // Local IPC (UDS, or loopback TCP) needs no transport security: on Unix the
      // run directory is created with 0700 permissions, so only the Core's user can
      // reach this socket. This mirrors the client credentials CoreHandle defaults to.
      grpc.ServerCredentials.createInsecure(),
      (err: Error | null, port: number) => {
        if (err) {
          reject(err);
          return;
        }
        resolve(port);
      },
    );
  });
}

/** Parses entrypoint from plugin.toml or direct script path. */
function resolvePluginEntrypoint(targetPath: string): string {
  const resolved = path.resolve(targetPath);
  if (fs.statSync(resolved).isDirectory()) {
    const tomlPath = path.join(resolved, "plugin.toml");
    if (fs.existsSync(tomlPath)) {
      return resolvePluginEntrypoint(tomlPath);
    }
    return path.join(resolved, "index.js");
  }

  if (resolved.endsWith(".toml")) {
    const content = fs.readFileSync(resolved, "utf-8");
    const manifest = parseToml(content);
    const plugin = manifest.plugin;
    const entrypoint =
      plugin && typeof plugin === "object" && !Array.isArray(plugin) && "entrypoint" in plugin
        ? plugin.entrypoint
        : undefined;
    if (typeof entrypoint !== "string" || entrypoint.trim() === "") {
      throw new Error(`${resolved} must define a nonempty [plugin].entrypoint`);
    }
    const dir = path.dirname(resolved);
    const candidate = path.join(dir, entrypoint);
    // If typescript source file was referenced (.ts), try dist equivalent or require ts directly
    if (candidate.endsWith(".ts")) {
      const jsCandidate = candidate.replace(/\.ts$/, ".js");
      if (fs.existsSync(jsCandidate)) {
        return jsCandidate;
      }
      // The SDK workspace compiles plugins under dist/plugins, preserving their source paths.
      const distCandidate = path.join(
        dir, "../../dist/plugins", path.basename(dir), entrypoint.replace(/\.ts$/, ".js"),
      );
      if (fs.existsSync(distCandidate)) {
        return distCandidate;
      }
    }
    return candidate;
  }

  return resolved;
}

/**
 * Reads the configuration the Core saved for this plugin (`<dataDir>/config.json`).
 *
 * A missing file means the plugin was never configured. A malformed one fails startup:
 * running with an empty configuration would silently ignore what the operator saved.
 */
function readStoredConfig(dataDir: string): Record<string, any> {
  const file = path.join(dataDir, "config.json");
  if (!fs.existsSync(file)) {
    return {};
  }
  const parsed = JSON.parse(fs.readFileSync(file, "utf8"));
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error(`${file} must contain a JSON object`);
  }
  return parsed;
}

/** Loads and instantiates the Plugin instance. */
async function loadPlugin(targetPath: string): Promise<Plugin> {
  const entrypoint = resolvePluginEntrypoint(targetPath);
  if (!fs.existsSync(entrypoint)) {
    throw new Error(`Plugin entrypoint not found: ${entrypoint}`);
  }

  const imported = await import(pathToFileURL(entrypoint).href);
  let target: any = imported.default || imported;
  while (target && target.default && typeof target !== "function") {
    target = target.default;
  }

  if (typeof target === "function") {
    return new target();
  }
  if (target && typeof target.meta === "function") {
    return target;
  }

  for (const val of Object.values(imported)) {
    let candidate: any = val;
    while (candidate && candidate.default && typeof candidate !== "function") {
      candidate = candidate.default;
    }
    if (typeof candidate === "function") {
      try {
        const inst = new candidate();
        if (inst instanceof Plugin || typeof inst.meta === "function") {
          return inst;
        }
      } catch (_) {}
    }
  }

  throw new Error(`Invalid plugin export in ${entrypoint}`);
}

async function main(): Promise<void> {
  // 1. Parse arguments and environment
  const args = process.argv.slice(2);
  let pluginArg: string | undefined;
  let socketArg: string | undefined;

  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--plugin" && i + 1 < args.length) {
      pluginArg = args[++i];
    } else if (args[i] === "--socket" && i + 1 < args.length) {
      socketArg = args[++i];
    }
  }

  const pluginPath = pluginArg;

  if (!pluginPath) {
    console.error("Error: No plugin specified via --plugin");
    process.exit(1);
  }

  const socketPath = path.resolve(
    socketArg || process.env.KANON_HOST_SOCK || "./run/host_ts.sock",
  );
  const coreSockPath = process.env.KANON_CORE_SOCK;
  const hostId = process.env.KANON_HOST_ID || "host_ts";

  // 2. Load the plugin and assemble its context together with the Core client.
  //
  //    The context is built here, before `onLoad`, because the Core handle is
  //    process-wide state: the host owns exactly one channel to the Core, hands the
  //    same handle to the plugin for its whole lifetime, and closes it on shutdown.
  //    A plugin cannot create this handle later by itself without hand-rolling gRPC,
  //    which is precisely what the SDK exists to prevent. Building both objects in
  //    one place also keeps the standalone decision honest: when the Core socket is
  //    absent or dead, `ctx.core` is left undefined and the plugin is told so by the
  //    log, rather than receiving a client that can only fail on first use.
  const plugin = await loadPlugin(pluginPath);
  const meta = plugin.meta();

  const dataDir = path.resolve(`./data/plugins/${meta.id}`);
  fs.mkdirSync(dataDir, { recursive: true });

  const coreHandle = await connectCore(coreSockPath, { hostId, pluginId: meta.id });
  const ctx: PluginContext = {
    dataDir,
    // The Core pushes configuration only when the operator changes it, so the last saved
    // configuration must be read here or the plugin would start unconfigured after a restart.
    config: readStoredConfig(dataDir),
    core: coreHandle,
  };
  // Set before onLoad: plugins commonly override onLoad without calling super, and the SDK's
  // event objects reach Core through plugin.context.
  plugin.context = ctx;
  await plugin.onLoad(ctx);

  // 3. Prepare IPC socket directory and clean up stale socket
  fs.mkdirSync(path.dirname(socketPath), { recursive: true });
  await prepareEndpoint(socketPath);

  // 4. Load gRPC IDL definitions. The descriptor is memoized in the SDK and also
  //    backs the CoreHandle client, so host server and Core client always agree on
  //    the IDL and its field naming.
  const kanonV1 = (loadKanonProto() as any).kanon.plugin.v1;

  // 5. Initialize gRPC server and register services
  const server = new grpc.Server({ interceptors: process.platform === "win32" || !!process.env.KANON_IPC_TOKEN ? [serverAuth(ipcToken())] : [] });
  let configVersion = 0;
  let reloadingConfig = false;

  server.addService(kanonV1.PluginHostService.service, {
    Ping: (call: any, callback: any) => {
      callback(null, { timestamp: call.request.timestamp });
    },
    ReloadPluginConfig: async (call: any, callback: any) => {
      const version = Number(call.request.version || 0);
      const currentVersion = configVersion;
      if (reloadingConfig) {
        callback(null, {
          success: false,
          error_message: "A configuration reload is still running",
          applied_version: currentVersion,
        });
        return;
      }
      if (version > 0 && version <= currentVersion) {
        callback(null, {
          success: false,
          error_message: `Stale config version ${version}: current is ${currentVersion}`,
          applied_version: currentVersion,
        });
        return;
      }
      const previous = ctx.config;
      let cancelled = call.cancelled;
      const onCancelled = () => {
        cancelled = true;
        ctx.config = previous;
      };
      if (cancelled) return;
      // JavaScript cannot cancel a running plugin Promise. Restore the cache immediately,
      // but keep this reload exclusive until its callback settles, so a late completion or
      // rejection cannot overwrite a newer candidate after Core has rolled this one back.
      reloadingConfig = true;
      call.once("cancelled", onCancelled);
      try {
        if (call.request.config) {
          ctx.config = fromProtoStruct(call.request.config);
          await plugin.onConfigReload(ctx.config);
        }
        if (cancelled || call.cancelled) {
          ctx.config = previous;
          return;
        }
        configVersion = version;
        callback(null, { success: true, error_message: "", applied_version: version });
      } catch (err: any) {
        // A rejected reload leaves the plugin on the configuration it accepted last.
        ctx.config = previous;
        if (!cancelled && !call.cancelled) {
          callback(null, {
            success: false,
            error_message: `onConfigReload failed: ${err?.message || err}`,
            applied_version: currentVersion,
          });
        }
      } finally {
        call.removeListener("cancelled", onCancelled);
        reloadingConfig = false;
      }
    },
    GetPluginMeta: (call: any, callback: any) => {
      callback(null, { plugins: [plugin.meta()] });
    },
    InvokeAction: async (call: any, callback: any) => {
      // Plugins written as plain objects may not implement actions at all.
      if (typeof (plugin as any).onInvokeAction !== "function") {
        callback(null, {
          success: false,
          error_message: `plugin '${call.request.plugin_id}' declares no actions`,
        });
        return;
      }
      try {
        const parameters = call.request.parameters ? fromProtoStruct(call.request.parameters) : {};
        callback(null, await plugin.onInvokeAction(call.request.action, parameters));
      } catch (err: any) {
        callback(null, { success: false, error_message: err?.message || "Action error" });
      }
    },
  });

  server.addService(kanonV1.MessagePipelineService.service, {
    OnPreFilter: async (call: any, callback: any) => {
      try {
        const res = await plugin.onPreFilter(call.request);
        if (!res) {
          callback(null, {
            action: "PASS",
            modified_text: "",
            reply_messages: [],
          });
        } else {
          callback(null, res);
        }
      } catch (err: any) {
        callback({
          code: grpc.status.INTERNAL,
          message: err?.message || "PreFilter error",
        });
      }
    },
    OnExecuteCommand: async (call: any, callback: any) => {
      try {
        const res = await plugin.onExecuteCommand(call.request);
        callback(null, res);
      } catch (err: any) {
        callback(null, {
          success: false,
          replies: [],
          error_message: err?.message || "Command error",
        });
      }
    },
    OnCallTool: async (call: any, callback: any) => {
      try {
        const res = await plugin.onCallTool(call.request);
        callback(null, res);
      } catch (err: any) {
        callback(null, {
          call_id: call.request.call_id,
          success: false,
          error_message: err?.message || "Tool error",
        });
      }
    },
    OnEvent: async (call: any, callback: any) => {
      try {
        await plugin.onEvent(call.request);
        callback(null, { received: true });
      } catch (err: any) {
        callback(null, { received: false });
      }
    },
    OnDecorateReply: async (call: any, callback: any) => {
      if (typeof (plugin as any).onDecorateReply !== "function") {
        callback(null, { modified: false, segments: [] });
        return;
      }
      try {
        callback(null, await plugin.onDecorateReply(call.request));
      } catch (err: any) {
        // Surfaced as an RPC error: Core then keeps the reply unchanged.
        callback({ code: grpc.status.INTERNAL, message: err?.message || "Decorate error" });
      }
    },
    OnPrepareTurn: async (call: any, callback: any) => {
      if (typeof (plugin as any).onPrepareTurn !== "function") {
        callback(null, { text: "" });
        return;
      }
      try {
        callback(null, await plugin.onPrepareTurn(call.request));
      } catch (err: any) {
        // Surfaced as an RPC error: Core then answers without this plugin's context.
        callback({ code: grpc.status.INTERNAL, message: err?.message || "Prepare error" });
      }
    },
    OnLlmRequest: async (call: any, callback: any) => {
      if (typeof (plugin as any).onLlmRequest !== "function") {
        callback(null, {});
        return;
      }
      try {
        callback(null, await plugin.onLlmRequest(call.request));
      } catch (err: any) {
        // Surfaced as an RPC error: Core then continues from the prompt it had.
        callback({ code: grpc.status.INTERNAL, message: err?.message || "LlmRequest error" });
      }
    },
    OnHttpRequest: async (call: any, callback: any) => {
      if (typeof (plugin as any).onHttpRequest !== "function") {
        callback(null, { status: 404, headers: [], body: Buffer.alloc(0) });
        return;
      }
      try {
        callback(null, await plugin.onHttpRequest(call.request));
      } catch (err: any) {
        callback({ code: grpc.status.INTERNAL, message: err?.message || "HttpRequest error" });
      }
    },
    OnDeliverMessage: async (call: any, callback: any) => {
      try {
        const res = await plugin.onDeliverMessage(call.request);
        callback(null, res);
      } catch (err: any) {
        callback(null, {
          success: false,
          message_id: "",
          error_message: err?.message || "Deliver error",
        });
      }
    },
  });

  // 6. Bind to IPC endpoint
  const bindAddress = process.platform === "win32" ? "127.0.0.1:0" : `unix:${socketPath}`;
  let endpointIdentity: fs.Stats;
  try {
    const port = await bindHostServer(server, bindAddress);
    if (process.platform === "win32") fs.writeFileSync(socketPath, `127.0.0.1:${port}`, { flag: "wx" });
    endpointIdentity = fs.lstatSync(socketPath);
    console.log(`Kanon TypeScript Host running on ${socketPath}`);
  } catch (err) {
    console.error(`Failed to bind socket ${bindAddress}:`, err);
    process.exit(1);
  }

  // Install shutdown before registration: even a Core that never answers must not prevent a
  // signal from unloading the plugin and releasing the endpoint.
  let shuttingDown = false;
  let stopWatchdog: (() => void) | undefined;
  const shutdown = async (exitCode = 0) => {
    // Signals and the watchdog can race; the plugin must be unloaded exactly once.
    if (shuttingDown) return;
    shuttingDown = true;
    stopWatchdog?.();

    let finished = false;
    const finish = () => {
      if (finished) return;
      finished = true;
      clearTimeout(timeout);
      try {
        // The host owns the shared channel, so it is closed here and nowhere else.
        coreHandle?.close();
        if (fs.existsSync(socketPath)) {
          const current = fs.lstatSync(socketPath);
          if (current.ino === endpointIdentity.ino && current.dev === endpointIdentity.dev) fs.unlinkSync(socketPath);
        }
      } catch (error) {
        console.error("[kanon-host] shutdown cleanup failed:", error);
      } finally {
        process.exit(exitCode);
      }
    };
    // A hung onUnload or outstanding RPC must not keep an orphan platform adapter alive.
    const timeout = setTimeout(() => {
      console.warn(`[kanon-host] shutdown did not finish within ${SHUTDOWN_TIMEOUT_MS}ms; forcing stop`);
      server.forceShutdown();
      finish();
    }, SHUTDOWN_TIMEOUT_MS);

    try {
      await plugin.onUnload();
    } catch (error) {
      console.error("[kanon-host] plugin teardown failed:", error);
    }
    server.tryShutdown(finish);
  };

  process.on("SIGINT", () => void shutdown(0));
  process.on("SIGTERM", () => void shutdown(0));

  // Announce only once the endpoint serves callbacks. Registration has a finite RPC deadline;
  // failure is logged because the supervisor may already know this host independently.
  if (coreHandle) {
    try {
      await coreHandle.registerHost(hostId, socketPath, [meta.id]);
      console.log(`Registered with Core as host '${hostId}'`);
    } catch (err: any) {
      console.warn(`Failed to register with Core: ${err?.message || err}`);
    }
  }
  if (shuttingDown) return;

  // 9. Watch the Core: a host whose Core is gone must stop, otherwise it keeps serving its
  //    platform and double-handles every message once a new Core starts.
  stopWatchdog = coreHandle
    ? startCoreWatchdog(coreHandle, {
        onLost: (reason) => {
          console.warn(`[kanon-host] ${reason}`);
          void shutdown(1);
        },
      })
    : undefined;
}

main().catch((err) => {
  console.error("Fatal error in Kanon TypeScript Host:", err);
  process.exit(1);
});
