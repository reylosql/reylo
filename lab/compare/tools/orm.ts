// What the ORM adapters share. An ORM types the query a user builds in TypeScript, so each probe is written in the ORM's own API (its raw SQL fragment where the API has no way to say it, untyped as the ORM leaves it), and the type checker reads back what the ORM types it as. A parameter is a value passed to the builder: like SafeQL's, it counts as typed when the type check refuses a value of the wrong type, and as taking null when it accepts one.
import { symlinkSync, writeFileSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import ts from "typescript";
import type { Db, Kind } from "../probes.ts";
import { failed, rewrite, type Answer, type Statement, type Tool, type TypedValue, type Urls } from "./lib.ts";

export const root = resolve(import.meta.dirname, "..");
export const versionOf = (pkg: string): string => JSON.parse(readFileSync(join(root, "node_modules", pkg, "package.json"), "utf8")).version;

// `value`: the code evaluates to the column's value itself, as a count does, rather than to rows. `pick`: the path to the column's value in each row, for an ORM that nests it (`_max.id`); `paths`: the same for each of several columns, by the probe's name for it.
export type Code = string | { code: string; value?: true; pick?: string; paths?: Record<string, string> };
// A function writes the query from the probe's SQL, for an ORM whose only way to say it is its raw SQL. `null`: the ORM has no query this probe can fairly stand for, and the comment beside it says why.
export type Entry = Code | Partial<Record<Db, Code>> | ((sql: string) => Code) | null;
export interface Project {
  dir: string;
  // What every query file starts with: the imports and the client, declared, since nothing runs.
  prelude: string;
  // What the step that made the schema left to commit.
  wrote: string[];
}
export interface OrmSpec {
  id: string;
  name: string;
  version: string;
  dbs: Db[];
  cardinalityNote: string;
  // The schema as the ORM's users get it from an existing database, made once per database: a later run with no server reads the same copy, as the ORM's type check does.
  project(db: Db, urls: Urls): Project;
  // Each probe in the ORM's API, by `<group>:<probe>`.
  queries: Record<string, Entry>;
  // What marks a query as the ORM's raw SQL rather than its typed API.
  raw: RegExp;
  compilerOptions?: ts.CompilerOptions;
}

const OPTIONS: ts.CompilerOptions = { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext, moduleResolution: ts.ModuleResolutionKind.Bundler, strict: true, skipLibCheck: true, noEmit: true, allowImportingTsExtensions: true, types: [] };

export function projectDir(dir: string) {
  symlinkSync(join(root, "node_modules"), join(dir, "node_modules"));
  writeFileSync(join(dir, "package.json"), '{"name":"p","private":true,"type":"module"}\n');
}

const kindToValue = (k?: Kind) => (k === "num" ? "number" : k === "time" ? "Date" : k === "bool" ? "boolean" : "string");
// The values a caller would pass for a parameter, the most natural first: a timestamp may be passed as a Date or as text.
const candidates = (s: Statement, i: number): string[] => {
  const want = s.probe.params?.[i];
  if (want?.value) return [want.value];
  return [...new Set((want?.kinds ?? ["text"]).map(kindToValue))];
};

function resolveCode(entry: Entry | undefined, db: Db, sql: string): { code: string; value: boolean; pick?: string; paths?: Record<string, string> } | undefined {
  if (entry === undefined || entry === null) return undefined;
  if (typeof entry === "function") return resolveCode(entry(sql), db, sql);
  if (typeof entry === "string") return { code: entry, value: false };
  if ("code" in entry) return { code: entry.code as string, value: !!entry.value, pick: entry.pick as string | undefined, paths: entry.paths as Record<string, string> | undefined };
  const c = (entry as Partial<Record<Db, Code>>)[db];
  return c === undefined ? undefined : resolveCode(c, db, sql);
}

// One file of queries, one to a line, each parameter a declared value of the given type. Returns each query's line.
function writeQueries(path: string, prelude: string, items: { name: string; code: string; values: string[] }[]) {
  const head = `${prelude}\n`;
  const decls = items.flatMap((it) => it.values.map((v, k) => `declare const ${it.name}_p${k + 1}: ${v};`));
  const lines = items.map((it) => {
    const code = rewrite(it.code, (n) => `${it.name}_p${n}`);
    return `export const ${it.name} = async () => ${code.startsWith("{") ? code : `(${code})`};\nexport type ${it.name}_R = Awaited<ReturnType<typeof ${it.name}>>;`;
  });
  const text = `${head}${decls.join("\n")}\n${lines.join("\n")}\n`;
  writeFileSync(path, text);
  const at: Record<string, number> = {};
  text.split("\n").forEach((l, i) => { const m = /^export const (\w+) = /.exec(l); if (m) at[m[1]] = i; });
  return at;
}

function compile(dir: string, file: string, options: ts.CompilerOptions) {
  const path = join(dir, file);
  const program = ts.createProgram([path], { ...OPTIONS, ...options });
  const source = program.getSourceFile(path)!;
  const errors = new Map<number, string[]>();
  for (const d of ts.getPreEmitDiagnostics(program, source)) {
    if (d.file !== source || d.start === undefined) continue;
    const line = source.getLineAndCharacterOfPosition(d.start).line;
    errors.set(line, [...(errors.get(line) ?? []), ts.flattenDiagnosticMessageText(d.messageText, " ")]);
  }
  return { program, source, errors };
}

// A type as the lab reads every tool's: its kind, whether it admits null, and the fields of the object it holds.
function readType(checker: ts.TypeChecker, type: ts.Type, readFields = true): TypedValue {
  const kinds = new Set<Kind>();
  const fields = new Set<string>();
  const fieldTypes: Record<string, { kind: Kind; nullable: boolean }> = {};
  const labels = new Set<string>();
  let nullable = false;
  let hasArray = false;
  let hasNonArray = false;
  const visit = (t: ts.Type, top: boolean, depth: number): void => {
    if (t.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown)) { if (top) { nullable = true; hasNonArray = true; } kinds.add("any"); return; }
    if (t.isUnion()) return t.types.forEach((x) => visit(x, top, depth));
    if (t.flags & (ts.TypeFlags.Null | ts.TypeFlags.Undefined | ts.TypeFlags.Void)) { if (top) nullable = true; return; }
    if (t.flags & (ts.TypeFlags.NumberLike | ts.TypeFlags.BigIntLike)) { if (top) hasNonArray = true; return void kinds.add("num"); }
    if (t.isStringLiteral()) { if (top) { labels.add(t.value); hasNonArray = true; } return void kinds.add("text"); }
    if (t.flags & ts.TypeFlags.StringLike) { if (top) hasNonArray = true; return void kinds.add("text"); }
    if (t.flags & ts.TypeFlags.BooleanLike) { if (top) hasNonArray = true; return void kinds.add("bool"); }
    const name = (t.aliasSymbol ?? t.getSymbol())?.getName() ?? "";
    if (name === "Date") { if (top) hasNonArray = true; return void kinds.add("time"); }
    if (/Decimal$/.test(name)) { if (top) hasNonArray = true; return void kinds.add("num"); }
    if (checker.isArrayType(t) || checker.isTupleType(t)) { if (top) hasArray = true; kinds.add("json"); const el = checker.getTypeArguments(t as ts.TypeReference)[0]; if (el && depth < 3) visit(el, false, depth + 1); return; }
    if (t.flags & ts.TypeFlags.Object) {
      if (top) hasNonArray = true;
      kinds.add("json");
      for (const p of checker.getPropertiesOfType(t)) {
        fields.add(p.getName());
        if (readFields && depth <= 1) {
          const ft = readType(checker, checker.getTypeOfSymbol(p), false);
          fieldTypes[p.getName()] = { kind: ft.kind, nullable: ft.nullable || !!(p.flags & ts.SymbolFlags.Optional) };
        }
      }
      return;
    }
    if (top) hasNonArray = true;
    kinds.add("any");
  };
  visit(type, true, 0);
  const kind: Kind = kinds.has("any") ? "any" : kinds.has("json") ? "json" : kinds.values().next().value ?? "any";
  const text = checker.typeToString(type, undefined, ts.TypeFormatFlags.NoTruncation | ts.TypeFormatFlags.InTypeAlias);
  return { type: text, kind, nullable, ...(hasArray && !hasNonArray ? { array: true } : {}), fields: fields.size ? [...fields] : undefined, ...(Object.keys(fieldTypes).length ? { fieldTypes } : {}), labels: labels.size ? [...labels] : undefined };
}
const untyped = (t: ts.Type) => !!(t.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown));

export function ormTool(spec: OrmSpec): Tool {
  const projects = new Map<Db, Project>();
  return {
    id: spec.id,
    orm: true,
    name: spec.name,
    version: spec.version,
    dbs: spec.dbs,
    cardinalityNote: spec.cardinalityNote,
    noConnectNote: "It types each query from the schema kept in your code, and never asks the database.",
    async analyze(db, statements, urls) {
      if (!projects.has(db)) projects.set(db, spec.project(db, urls));
      const project = projects.get(db)!;
      const answers: Record<string, Answer> = {};
      type Item = { s: Statement; name: string; code: string; value: boolean; paths?: Record<string, string>; n: number; pick: number[]; options: string[][] };
      const items: Item[] = [];
      statements.forEach((s, i) => {
        // The run's check for a database to reach names its statement by a bare key: the smoke query.
        const [group, probe] = s.key.includes(":") ? s.key.split(":").slice(1) : ["smoke", s.probe.id];
        // The row count is picked per call (`findMany`, `findFirst`, `executeTakeFirst`), never inferred, and what only a database knows is never asked: it never connects.
        if (group === "cardinality") return void (answers[s.key] = { ok: true, params: [], columns: [], cardinality: null });
        if (group === "from-the-database") return;
        const entry = spec.queries[`${group}:${probe}`];
        if (entry === null) return;
        const c = resolveCode(entry, db, s.sql);
        if (!c) throw new Error(`${spec.id}: no query for ${group}:${probe} on ${db}`);
        const n = new Set([...c.code.matchAll(/\$(\d+)/g)].map((m) => m[1])).size;
        const options = Array.from({ length: n }, (_, k) => candidates(s, k));
        const paths = c.paths ?? (c.pick ? { [s.probe.columns?.[0]?.name ?? c.pick.split(".").pop()!]: c.pick } : undefined);
        items.push({ s, name: `q${i}`, code: c.code, value: c.value, paths, n, pick: options.map(() => 0), options });
      });
      if (!items.length) return { answers, wrote: project.wrote };
      const valuesOf = (it: Item) => it.pick.map((p, k) => it.options[k][p]);

      // The right values, each parameter's next candidate tried where the type check refuses the first.
      let right!: ReturnType<typeof compile>;
      let lines: Record<string, number> = {};
      for (let round = 0; round < 3; round++) {
        lines = writeQueries(join(project.dir, "right.ts"), project.prelude, items.map((it) => ({ name: it.name, code: it.code, values: valuesOf(it) })));
        right = compile(project.dir, "right.ts", spec.compilerOptions ?? {});
        let moved = false;
        for (const it of items) if (right.errors.has(lines[it.name])) it.pick = it.pick.map((p, k) => { if (p + 1 < it.options[k].length) { moved = true; return p + 1; } return p; });
        if (!moved) break;
      }
      const checker = right.program.getTypeChecker();
      const aliases = new Map<string, ts.Type>();
      for (const st of right.source.statements) if (ts.isTypeAliasDeclaration(st)) aliases.set(st.name.text, checker.getTypeAtLocation(st.name));

      // For each parameter: is a value of the wrong type refused, and is a null refused?
      const perParam = items.flatMap((it) => Array.from({ length: it.n }, (_, k) => ({ it, k })));
      const variant = (file: string, make: (v: string) => string) => {
        const at = writeQueries(join(project.dir, file), project.prelude, perParam.map(({ it, k }) => ({ name: `${it.name}_${k}`, code: it.code, values: valuesOf(it).map((v, j) => (j === k ? make(v) : v)) })));
        const { errors } = compile(project.dir, file, spec.compilerOptions ?? {});
        return (name: string) => errors.has(at[name]);
      };
      const wrongRefused = perParam.length ? variant("wrong.ts", () => "boolean") : () => false;
      const nullRefused = perParam.length ? variant("nulls.ts", (v) => `${v} | null`) : () => false;

      for (const it of items) {
        const raw = spec.raw.test(it.code) ? { raw: true } : {};
        const errs = right.errors.get(lines[it.name]);
        if (errs) { answers[it.s.key] = { ...failed(errs.join("; ")), code: it.code, ...raw }; continue; }
        const params: TypedValue[] = Array.from({ length: it.n }, (_, k) => {
          const refused = wrongRefused(`${it.name}_${k}`);
          const value = valuesOf(it)[k];
          const want = it.s.probe.params?.[k];
          return { type: value, kind: refused ? (want?.kinds?.[0] ?? (value === "number" ? "num" : value === "Date" ? "time" : "text")) : "any", nullable: !nullRefused(`${it.name}_${k}`), checksValues: true, wrongValuePasses: refused ? undefined : "boolean" };
        });
        let result = aliases.get(`${it.name}_R`)!;
        let columns: (TypedValue & { name: string })[];
        if (it.value) columns = [{ name: "v", ...readType(checker, result) }];
        else if (it.paths) {
          const row = checker.isArrayType(result) ? checker.getTypeArguments(result as ts.TypeReference)[0] : result;
          columns = Object.entries(it.paths).map(([name, path]) => {
            let t = row;
            for (const key of path.split(".")) t = checker.getTypeOfSymbol(checker.getPropertyOfType(checker.getNonNullableType(t), key)!);
            return { name, ...readType(checker, t) };
          });
        } else {
          if (checker.isArrayType(result)) result = checker.getTypeArguments(result as ts.TypeReference)[0];
          // A row the ORM leaves untyped: nothing of the query was read.
          if (untyped(result) && it.s.probe.columns?.length) { answers[it.s.key] = { ok: true, params, columns: null, cardinality: null, code: it.code, ...raw, untyped: checker.typeToString(result) }; continue; }
          columns = checker.getPropertiesOfType(result).map((p) => ({ name: p.getName(), ...readType(checker, checker.getTypeOfSymbol(p)) }));
          // The support check counts the columns it selects; TypeORM's `find` types every column of the entity whatever it selects.
          if (it.s.probe.id === "smoke") columns = columns.filter((c) => it.s.probe.columns!.some((w) => w.name === c.name));
        }
        answers[it.s.key] = { ok: true, params, columns, cardinality: null, code: it.code, ...raw };
      }
      return { answers, wrote: project.wrote };
    },
  };
}
