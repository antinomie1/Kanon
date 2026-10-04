/** Bundle the SDK codec once, leaving DSH peers resolved from the host profile. */
import { execFileSync } from "node:child_process";
import { mkdir, copyFile } from "node:fs/promises";

execFileSync(process.execPath, ["build", "index.ts", "--target", "node", "--external", "@deepseek-ai/*",
  "--external", "@grpc/*", "--outfile", "dist/index.js"], { stdio: "inherit" });
await mkdir("dist/proto/kanon/v1", { recursive: true });
for (const name of ["agent", "plugin"]) {
  await copyFile(`../../proto/kanon/v1/${name}.proto`, `dist/proto/kanon/v1/${name}.proto`);
}
