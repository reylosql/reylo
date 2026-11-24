// `await migrate()` on a server (src/migrate.ts): `reylo migrate deploy` run by the binary, in the
// working directory, as the command line runs it there, its `--json` result read back. It resolves to
// the migrations it ran, and rejects with the deploy's own problems and their fixes.
import { spawn } from "node:child_process";
import type { MigrateOptions } from "./build-time.ts";
import { nativeBinary } from "./native.ts";

type Deploy = {
  ok: boolean;
  error?: string;
  result?: { migrations?: { name: string; state: string; error?: string }[] };
  problems?: { title: string; detail?: string[]; sql?: string; notes?: string[]; fixes?: { what: string; commands: string[] }[] }[];
  log?: string[];
};

/** `reylo migrate deploy`, on the database `options.url` names or the project's. */
export async function migrate(options: MigrateOptions): Promise<string[]> {
  const args = ["migrate", "deploy", "--json", ...(options.url !== undefined ? ["--url", options.url] : [])];
  const child = spawn(nativeBinary(), args, { cwd: process.cwd(), env: process.env, windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
  let out = "";
  let err = "";
  child.stdout.setEncoding("utf8").on("data", (d: string) => (out += d));
  child.stderr.setEncoding("utf8").on("data", (d: string) => (err += d));
  await new Promise<void>((done, fail) => {
    child.once("error", fail);
    child.once("close", () => done());
  });
  let deploy: Deploy;
  try {
    deploy = JSON.parse(out) as Deploy;
  } catch {
    throw new Error(`reylo migrate deploy failed: ${(err || out).trim()}`);
  }
  if (!deploy.ok) {
    // A migration that failed is said so, its refusal (what of it is in place, and what to run) or its error leading.
    // Each refusal says what it is about as the command line does under its title: the change it
    // found (a column added by hand), or the SQL that makes it, so that an app's log names it.
    const failed = deploy.result?.migrations?.find((m) => m.state === "failed");
    if (deploy.problems?.length) {
      const lines = deploy.problems.flatMap((p) => [
        p.title,
        ...(p.detail ?? []).map((d) => `  ${d}`),
        ...(p.sql ?? "").split("\n").filter((l) => l.trim()).map((l) => `    ${l}`),
        ...(p.notes ?? []).map((n) => `  ${n}`),
        ...(p.fixes ?? []).map((f) => `  ${f.what}: ${f.commands.join(", then ")}`),
      ]);
      throw new Error(`reylo migrate deploy ${failed ? "failed" : "refused"}: ${lines.join("\n")}`);
    }
    // With no refusal, the log follows, which says what ran before it.
    const log = (deploy.log ?? []).join("\n");
    const lead = failed ? `${failed.name}: ${failed.error ?? "failed"}` : (deploy.error ?? log);
    throw new Error(`reylo migrate deploy failed: ${lead}${failed && log ? `\n${log}` : ""}`);
  }
  return (deploy.result?.migrations ?? []).filter((m) => m.state === "applied").map((m) => m.name);
}
