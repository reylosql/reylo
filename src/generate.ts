// `reylo generate typescript`, which npm's `reylo` command runs here rather than in the binary
// (src/cli.ts): every `.sql` file the paths name, written into one TypeScript module of an export
// each. An export is the query the module a build step writes for its file defines (src/loader.ts),
// typed as the content mapper declares it (src/mapper-server.ts), so code that imports the file
// runs with no build step of Reylo's and type-checks on any TypeScript, with no content mapper.
import { spawnSync } from "node:child_process";
import { readdirSync, readFileSync, statSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, extname, isAbsolute, matchesGlob, relative, resolve, sep } from "node:path";
import { styleText } from "node:util";
import { Session } from "./analyze.ts";
import { leadingComment, statementAfter, docComment } from "./doc-comment.ts";
import { compiledText, DEFINE, moduleDefinition, moduleImports, moduleInputOf, moduleLibraries, type ModuleInput } from "./loader.ts";
import { moduleTypes, reyloImport } from "./module-types.ts";
import { geojsonTypesProblem, libraryProblem, PROJECT_FILE } from "./project.ts";
import type { Lexed } from "./engine.ts";
import type { Library } from "./codecs.ts";

export const GENERATE_TYPESCRIPT_USAGE = `Usage: reylo generate typescript <file.sql | directory | 'glob'> ... --out <file.ts>

Writes every .sql file's query into one TypeScript module, typed by the project's database: an
export each, named after its file in camelCase, so get_user.sql is getUser, with its parameters'
and row's types as getUser.Params and getUser.Row. Two files with one name fail. A directory is
searched for .sql files, and a glob in quotes is expanded by Reylo. The module imports reylo's run
time and needs no build step or content mapper. Exits with 1 when any statement fails, writing
nothing.`;

/** The first line of the file the generator writes, which it looks for before it writes over one. */
const MARK = "// Written by `reylo generate typescript`";

/** The directories a directory's search leaves out: dependencies and build output, as `reylo generate rust-sqlx` does (rust/reylo-build/src/files.rs). */
const SKIPPED = new Set(["node_modules", "dist", "build", "out", "target", "coverage"]);

/** Names no export may take: JavaScript's reserved words, those of strict code and modules, and `reylo`, the module's own import of the run time. The module's other imports are capitalized, as no export is. */
const RESERVED = new Set(
  "break case catch class const continue debugger default delete do else enum export extends false finally for function if import in instanceof new null return super switch this throw true try typeof var void while with yield let static implements interface package private protected public await arguments eval undefined reylo".split(" "),
);

const paint = (format: Parameters<typeof styleText>[0], text: string) => styleText(format, text);

/** A path from `base`, written with `/` on every system. */
const shown = (base: string, path: string) => relative(base, path).split(sep).join("/") || ".";

/** Runs `reylo generate typescript` with the arguments after the generator's name, and answers with the exit code. */
export async function generateTypescript(args: string[]): Promise<number> {
  if (args.includes("--help") || args.includes("-h")) {
    console.log(GENERATE_TYPESCRIPT_USAGE);
    return 0;
  }
  const inputs: string[] = [];
  let out: string | undefined;
  for (let i = 0; i < args.length; i++) {
    const arg = args[i]!;
    if (arg === "--out" || arg === "-o") {
      out = args[++i];
      if (out === undefined) return usage("--out takes a file");
    } else if (arg.startsWith("--out=")) out = arg.slice("--out=".length);
    else if (arg.startsWith("-")) return usage(`unknown option ${arg}`);
    else inputs.push(arg);
  }
  if (!inputs.length) return usage("name the .sql files, the directories or a glob to read, as 'src/**/*.sql'");
  if (out === undefined) return usage("name the file to write with --out, as --out src/queries.ts");
  if (![".ts", ".mts"].includes(extname(out))) return usage(`--out names a .ts or .mts file, not ${out}`);

  const base = process.cwd();
  const result = await generate(inputs, resolve(base, out), base);
  for (const session of Session.opened()) await session.close();
  if (!result.ok) {
    for (const line of result.errors) console.error(`${paint(["bold", "red"], "✗")} ${line}`);
    return 1;
  }
  if (result.changed) console.log(`${paint("green", "✓")} Wrote ${shown(base, result.path)}`);
  else console.log(paint("dim", `» No change to ${shown(base, result.path)}`));
  return 0;
}

function usage(message: string): number {
  console.error(`reylo generate: ${message} (reylo generate typescript --help)`);
  return 2;
}

export type Generated = { ok: true; path: string; changed: boolean } | { ok: false; errors: string[] };

/**
 * Writes the module of every `.sql` file `inputs` name, from `base`, to `out`: all of them, or
 * nothing where any file cannot be read, named or typed, with every reason, each naming its file.
 */
export async function generate(inputs: string[], out: string, base: string): Promise<Generated> {
  const errors: string[] = [];
  const found = new Set<string>();
  for (const input of inputs) {
    const files = sqlFiles(input, base);
    if (typeof files === "string") errors.push(files);
    else for (const file of files) found.add(file);
  }
  if (errors.length) return { ok: false, errors };
  if (!found.size) return { ok: false, errors: [`no .sql file under ${inputs.join(", ")}`] };

  // Every file's export is named first, so that two files of one name fail before any is analyzed.
  const named = new Map<string, string>();
  for (const file of [...found].sort()) {
    const stem = file.slice(dirname(file).length + 1, -extname(file).length);
    const name = exportName(stem);
    if (name === undefined) errors.push(`${shown(base, file)}: ${JSON.stringify(stem)} is no export's name: name the file with letters, digits and underscores, starting with a letter, as get_user.sql`);
    else if (RESERVED.has(name)) errors.push(`${shown(base, file)}: ${name} is a word JavaScript reserves: rename the file`);
    else if (named.has(name)) errors.push(`${shown(base, named.get(name)!)} and ${shown(base, file)} are both the export ${name}: rename one`);
    else named.set(name, file);
  }
  if (errors.length) return { ok: false, errors };

  const queries: Query[] = [];
  for (const [name, file] of [...named].sort(([a], [b]) => (a < b ? -1 : 1))) {
    const query = await queryOf(name, file, base);
    if (Array.isArray(query)) errors.push(...query);
    else queries.push(query);
  }
  if (errors.length) return { ok: false, errors };
  const decimals = new Set(queries.flatMap((q) => q.imports.filter((i) => i.startsWith("import ReyloDecimal "))));
  if (decimals.size > 1) return { ok: false, errors: [`the queries' projects name different decimal libraries in js.codecs.decimal: generate each project's queries into a file of its own`] };
  // The module imports each codec's library, and names @types/geojson's types, from its own place,
  // which can be another package than the one the .sql files are in, beside which the build found them.
  const problems = [
    ...[...new Set(queries.flatMap((q) => q.libraries))].map((library) => libraryProblem(library, dirname(out))),
    queries.some((q) => q.geojson) ? geojsonTypesProblem(dirname(out)) : undefined,
  ].filter((problem) => problem !== undefined);
  if (problems.length) return { ok: false, errors: problems.map((problem) => `${shown(base, out)}: ${problem}`) };

  const from = inputs.map((input) => shown(dirname(out), resolve(base, input))).map((p) => (p === "." ? "its own directory" : p));
  const text = [
    `${MARK} from ${from.join(", ")}: edit the queries and run it again, never this file.`,
    `import * as reylo from "reylo";`,
    reyloImport(...queries.flatMap((q) => q.types)),
    ...new Set(queries.flatMap((q) => q.imports)),
    ...queries.map((q) => `\n${q.text}`),
    ``,
  ].join("\n");
  return write(out, text, shown(base, out));
}

type Query = { text: string; types: string[]; imports: string[]; libraries: Library[]; geojson: boolean };

/** One file's export: its definition and its types, with the libraries it imports; or why it cannot be written, each line naming the file. */
async function queryOf(name: string, file: string, base: string): Promise<Query | string[]> {
  const where = shown(base, file);
  let sql: string;
  try {
    sql = readFileSync(file, "utf8");
  } catch (e) {
    return [`${where}: cannot read it: ${(e as Error).message}`];
  }
  let input: ModuleInput;
  let session: Session;
  try {
    ({ input, session } = await moduleInputOf(file, sql));
  } catch (e) {
    // The build's errors start with the file's absolute path.
    const message = (e as Error).message;
    return [message.startsWith(file) ? `${where}${message.slice(file.length)}` : `${where}: ${message}`];
  }
  const { project } = session;
  let lexed: Lexed | undefined;
  try {
    lexed = await session.lex(sql);
  } catch {}
  const projectFile = project.file ? shown(base, project.file) : PROJECT_FILE;
  // The types the type check declares, and every error it would report in the file: the generated
  // module is the type check's only reading of it.
  const types = moduleTypes(sql, input.analysis, input.dialect, lexed, project.strictParams, projectFile, project.codecs, project.allowNestedArrays && input.dialect === "postgres");
  if (types.diagnostics.length) return types.diagnostics.map((d) => `${where}:${place(sql, d.start)}: ${d.message}`);
  const { imports } = moduleImports(input);
  const definition = moduleDefinition(input, { raw: JSON.stringify(input.raw), sql: JSON.stringify(compiledText(input)), params: JSON.stringify(input.params), checks: "reylo.checks", codecRuntime: "reylo.codecRuntime" }, true);
  const standard = [types.columns === undefined ? "" : `readonly columns: ${types.columns}`, input.analysis.ordered ? "readonly ordered: true" : ""].filter(Boolean);
  // The literal types go in the Query type directly rather than through the namespace's aliases,
  // so a hover shows the row's shape, as the content mapper declares it.
  const queryType = [
    `Query<${types.params}, ${types.row}, ${JSON.stringify(types.cardinality)}, ${JSON.stringify(input.dialect)}, ${types.queryError}> & {`,
    `  readonly source: string;`,
    ...(standard.length ? [`  readonly "~standard": { readonly query: { ${standard.join("; ")} } };`] : []),
    `}`,
  ].join("\n");
  const leading = leadingComment(sql, input.dialect);
  const notes = input.analysis.notes?.join("\n\n");
  const text = [
    `${docComment(leading.prose, statementAfter(sql, leading), notes)}export const ${name} = reylo.${DEFINE[input.dialect]}(${definition}) as ${queryType};`,
    `export declare namespace ${name} {`,
    `  type Params = ${indent(types.params)};`,
    `  type Row = ${indent(types.row)};`,
    `  type QueryError = ${indent(types.queryError)};`,
    `}`,
  ].join("\n");
  // A type names GeoJSON's only where the project's codecs say so, and outside the doc comments a
  // column's database comment writes, read in one pass with the strings as reyloImport reads them.
  const code = (t: string) => t.replace(/"(?:[^"\\]|\\.)*"|\/\*[\s\S]*?\*\//g, (m) => (m.startsWith('"') ? m : ""));
  const geojson = project.codecs.geometry !== undefined && [types.params, types.row, types.columns ?? ""].some((t) => code(t).includes(`import("geojson")`));
  return { text, types: [types.params, types.row, types.columns ?? ""], imports, libraries: [...moduleLibraries(input)], geojson };
}

/** `type` indented to sit inside the namespace, its first line where it starts. */
function indent(type: string): string {
  return type.replaceAll("\n", "\n  ");
}

/**
 * A file's stem as an export's name: its words in camelCase. Words are ASCII letters and digits,
 * which `_`, `-`, `.` and spaces separate and a capital starts (`get_user`, `get-user` and `GetUser`
 * are each `getUser`; `HTTPStatus` is `httpStatus`), as `reylo generate rust-sqlx` names a module
 * in snake_case (rust/reylo-build/src/generate.rs, `file_words`). A stem with any other character,
 * or whose name would start with a digit, has none.
 */
export function exportName(stem: string): string | undefined {
  if (!/^[A-Za-z0-9_\-. ]+$/.test(stem)) return undefined;
  const words = stem
    .split(/[_\-. ]+/)
    .filter(Boolean)
    .flatMap((part) => part.split(/(?<=[a-z0-9])(?=[A-Z])|(?<=[A-Z])(?=[A-Z][a-z])/))
    .map((w) => w.toLowerCase());
  const name = words.map((w, i) => (i === 0 ? w : w[0]!.toUpperCase() + w.slice(1))).join("");
  return /^[a-z]/.test(name) ? name : undefined;
}

/**
 * The `.sql` files `path` names, from `base`, as `reylo generate rust-sqlx` reads them
 * (rust/reylo-build/src/files.rs): the file itself, every `.sql` file under a directory (leaving out
 * dependencies, build output, hidden directories and what git ignores), or what a glob in quotes
 * matches, without regard to case, each directory it matches searched the same way; or why there
 * are none.
 */
export function sqlFiles(path: string, base: string): string[] | string {
  if (/[*?[]/.test(path)) {
    const problem = globProblem(path);
    if (problem) return `${path} is not a glob Reylo reads: ${problem}`;
    const pattern = resolve(base, path).split(sep).join("/");
    const root = resolve(base, globRoot(path));
    const matches = (p: string) => matchesGlob(p.split(sep).join("/").toLowerCase(), pattern.toLowerCase());
    // A file matches where it, or a directory it is in below the glob's root, does.
    return search(root).filter((file) => {
      for (let at = file; at.length > root.length; at = dirname(at)) if (matches(at)) return true;
      return false;
    });
  }
  const full = resolve(base, path);
  let stat;
  try {
    stat = statSync(full);
  } catch {
    return `${path} does not exist`;
  }
  return stat.isDirectory() ? search(full) : [full];
}

/**
 * Why `path` is no glob, by the rules of the glob crate `reylo generate rust-sqlx` reads one with,
 * so that both refuse the same typos rather than match nothing: a run of more than two `*`, a `**`
 * that is not a whole path component, or a `[` that no `]` in its path component closes after a
 * character (two for `[!`).
 */
export function globProblem(path: string): string | undefined {
  const separator = (c: string | undefined) => c === undefined || c === "/" || c === "\\";
  for (let i = 0; i < path.length; i++) {
    if (path[i] === "*") {
      let stars = 1;
      while (path[i + stars] === "*") stars++;
      if (stars > 2) return "wildcards are either regular `*` or recursive `**`";
      if (stars === 2 && !(separator(path[i - 1]) && separator(path[i + 2]))) return "recursive wildcards must form a single path component";
      i += stars - 1;
    } else if (path[i] === "[") {
      const from = path[i + 1] === "!" ? i + 3 : i + 2;
      const close = from <= path.length ? path.indexOf("]", from) : -1;
      const end = path.slice(i).search(/[/\\]/);
      if (close < 0 || (end >= 0 && close > i + end)) return "invalid range pattern";
      i = close;
    }
  }
  return undefined;
}

/** The directory a glob's matches are all under: its path up to the first wildcard. */
function globRoot(path: string): string {
  const wildcard = path.search(/[*?[]/);
  const before = path.slice(0, wildcard);
  const slash = Math.max(before.lastIndexOf("/"), before.lastIndexOf("\\"));
  return slash < 0 ? "" : before.slice(0, slash);
}

/** Every `.sql` file under `dir` that git does not ignore, sorted. */
function search(dir: string): string[] {
  const found: string[] = [];
  walk(dir, found);
  const ignored = gitIgnored(dir, found);
  return found.filter((f) => !ignored.has(f)).sort();
}

/** Every `.sql` file under `dir`, leaving out the directories `SKIPPED` names and hidden ones but `.server`, whose queries a framework keeps there. */
function walk(dir: string, out: string[]): void {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return;
  }
  for (const entry of entries) {
    const path = resolve(dir, entry.name);
    if (entry.isDirectory()) {
      if (!SKIPPED.has(entry.name) && (!entry.name.startsWith(".") || entry.name === ".server")) walk(path, out);
    } else if (entry.isFile() && extname(entry.name).toLowerCase() === ".sql") out.push(path);
  }
}

/** The files among `files` that git ignores: none where git is missing or `dir` is in no repository. */
function gitIgnored(dir: string, files: string[]): Set<string> {
  if (!files.length) return new Set();
  const r = spawnSync("git", ["-C", dir, "check-ignore", "--stdin", "-z"], { input: files.join("\0"), encoding: "utf8" });
  // 0: some are ignored; 1: none is; 128 or no status: not a repository, or no git.
  return r.status === 0 ? new Set(r.stdout.split("\0").filter(Boolean).map((f) => (isAbsolute(f) ? f : resolve(dir, f)))) : new Set();
}

/** The line and column, from 1, of a UTF-8 byte offset into `text`. */
function place(text: string, bytes: number): string {
  const before = Buffer.from(text, "utf8").subarray(0, bytes).toString("utf8");
  return `${before.split("\n").length}:${before.length - before.lastIndexOf("\n")}`;
}

/** Writes `text` to `path`, which messages call `name`, unless it already reads so; refuses to write over a file the generator did not write. */
function write(path: string, text: string, name: string): Generated {
  let old: string | undefined;
  try {
    old = readFileSync(path, "utf8");
  } catch {}
  if (old === text) return { ok: true, path, changed: false };
  if (old !== undefined && !old.startsWith(MARK)) return { ok: false, errors: [`${name} exists and reylo generate did not write it: move it, or name another file with --out`] };
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, text);
  return { ok: true, path, changed: true };
}
