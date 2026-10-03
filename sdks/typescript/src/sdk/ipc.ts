/** Windows loopback transport and request authentication shared by the host and SDK. */
import * as fs from "node:fs";
import { createConnection, isIP } from "node:net";
import { timingSafeEqual } from "node:crypto";
import * as grpc from "@grpc/grpc-js";

/** Requires the 32-byte credential injected by the supervisor's launch contract. */
export function ipcToken(): string {
  const token = process.env.KANON_IPC_TOKEN ?? "";
  if (!/^[0-9a-f]{64}$/.test(token)) throw new Error("Windows IPC requires a 32-byte KANON_IPC_TOKEN");
  return token;
}

/** Reads a literal loopback address; never lets an endpoint file cause a remote dial. */
export function loopbackEndpoint(file: string): string {
  const address = fs.readFileSync(file, "utf8").trim();
  const match = /^(127(?:\.\d{1,3}){3}|\[::1\]):([0-9]+)$/.exec(address);
  if (!match || !isIP(match[1].replace(/[\[\]]/g, "")) || Number(match[2]) < 1 || Number(match[2]) > 65535) {
    throw new Error("IPC endpoint must use a nonzero loopback port");
  }
  return address;
}

/** Sends authentication in initial HTTP/2 metadata for unary and streaming RPCs. */
export function clientAuth(token: string): grpc.Interceptor {
  return (options, nextCall) => new grpc.InterceptingCall(nextCall(options), {
    start(metadata, listener, next) {
      metadata.set("x-kanon-auth-token", token);
      next(metadata, listener);
    },
  });
}

/** Rejects unauthenticated calls before their request reaches plugin handlers. */
export function serverAuth(token: string): grpc.ServerInterceptor {
  return (_method, call) => new grpc.ServerInterceptingCall(call, {
    start(next) {
      next({
        onReceiveMetadata(metadata, forward) {
          const value = metadata.get("x-kanon-auth-token")[0];
          const supplied = Buffer.from(typeof value === "string" ? value : "");
          const expected = Buffer.from(token);
          if (expected.length === 64 && supplied.length === expected.length && timingSafeEqual(supplied, expected)) {
            forward(metadata);
          } else {
            call.sendStatus({ code: grpc.status.UNAUTHENTICATED, details: "Invalid or missing IPC token" });
          }
        },
      });
    },
  });
}


/** Reclaims only a refused socket endpoint; live, unknown, and changed endpoints fail closed. */
export async function prepareEndpoint(file: string): Promise<void> {
  let previous: fs.Stats;
  try {
    previous = fs.lstatSync(file);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return;
    throw error;
  }
  if (previous.isSymbolicLink() || (process.platform === "win32" ? !previous.isFile() : !previous.isSocket())) {
    throw new Error(`IPC endpoint has an invalid file type: ${file}`);
  }
  const target = process.platform === "win32" ? loopbackEndpoint(file) : undefined;
  await new Promise<void>((resolve, reject) => {
    const address = target ? /^(.*):([0-9]+)$/.exec(target)! : undefined;
    const probe = address
      ? createConnection({ host: address[1].replace(/[\[\]]/g, ""), port: Number(address[2]) })
      : createConnection({ path: file });
    const timer = setTimeout(() => {
      probe.destroy();
      reject(new Error(`IPC endpoint did not refuse the readiness probe: ${file}`));
    }, 100);
    probe.once("connect", () => {
      clearTimeout(timer);
      probe.destroy();
      reject(new Error(`IPC endpoint is active: ${file}`));
    });
    probe.once("error", (error: NodeJS.ErrnoException) => {
      clearTimeout(timer);
      probe.destroy();
      if (error.code === "ECONNREFUSED") resolve();
      else reject(error);
    });
  });
  const current = fs.lstatSync(file);
  // A second host may have bound while the probe was in flight. Never remove its endpoint.
  if (current.dev !== previous.dev || current.ino !== previous.ino || current.ctimeMs !== previous.ctimeMs) {
    throw new Error(`IPC endpoint changed during stale check: ${file}`);
  }
  fs.unlinkSync(file);
}
