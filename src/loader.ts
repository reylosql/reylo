// What every build step emits for a `.sql` file: a module that carries everything the query needs
// at run time (the dialect, the statement as the engine rewrote it for the database, the binding
// order, each parameter's type, the row count the analysis proved), so that the running code reads no file and asks no
// database. The analysis comes from `src/analyze.ts`: `reylo/.cache/` when it holds the statement for the
// current schema, else the database. A build that has neither fails, because a module built
// without the analysis would not do what its types say.
import { existsSync } from "node:fs";
import { dirname } from "node:path";
import { Session } from "./analyze.ts";
import { libraryProblem, sourceName } from "./project.ts";
import { rowSource } from "./row.ts";
import { querySchemas } from "./schema.ts";
import { columnKind } from "./values.ts";
import { jsonNumbers } from "./render.ts";
import { analysisWithBases, analysisWithNestedArrays } from "./engine-type.ts";
import { planLibraries, planOf, type Codecs, type Library, type Plan } from "./codecs.ts";
import type { Dialect } from "./execute.ts";
import { DIAGNOSTIC, type Analysis, type Lexed } from "./engine-core.ts";

export type SqlModule = {
  code: string;
  /** The `reylo/.cache/` entry the module was built from. A bundler watches it, so that the module is built again when the type check analyzes the statement anew (after a migration). */
  entry: string;
  /** The project file and the `.env` files that exist: the module's text depends on them too (`sqlcommenter`, the URL), so a bundler watches them as well. */
  watched: string[];
};

/** What the build steps of this process have said already: a build says once that it reads `reylo/.cache/` unchecked, however many modules it builds, by why it does. */
const said = new Set<string>();

/** One line on stderr, once a run, where a module is built from `reylo/.cache/` with no database to hold it to: the first such module's line, with the day its entry was written. */
function warnUnverified(line: string): void {
  const why = line.slice(0, line.lastIndexOf("; building from"));
  if (said.has(why)) return;
  said.add(why);
  console.warn(`reylo: ${line}`);
}

export async function sqlModule(path: string, sql: string): Promise<SqlModule> {
  const { input, entry, session } = await moduleInputOf(path, sql);
  return { code: moduleCode(input), entry, watched: session.project.watched.filter((f) => existsSync(f)) };
}

/**
 * What the module of the statement `sql`, read from the file at `path`, is written from, with the
 * session that analyzed it and the `reylo/.cache/` entry the analysis came from; or the error that
 * fails the build, which starts with `path`.
 */
export async function moduleInputOf(path: string, sql: string): Promise<{ input: ModuleInput; session: Session; entry: string }> {
  const session = await Session.for(dirname(path));
  let outcome = await session.analyze(sql);
  // A statement the database refuses may have been analyzed against a schema read a few seconds
  // before a migration (a session reads it again at most every 5 seconds, and SQLite's analysis
  // replays it): it is analyzed again when the schema has changed since.
  if (outcome.ok && outcome.analysis.diagnostics.some((d) => d.code === DIAGNOSTIC.refused) && (await session.recheck())) outcome = await session.analyze(sql);
  if (!outcome.ok) throw new Error(`${path}: ${outcome.message}`);
  if (outcome.unverified) warnUnverified(outcome.unverified);
  // Each domain written as its base type, which the run time decodes its values by.
  const analysis = analysisWithBases(outcome.analysis);
  // A statement whose placeholders break a rule fails the build: one the engine could not read,
  // named and positional mixed, or a parameter written more than once, since each parameter appears
  // once so that one place types it.
  const unread = analysis.diagnostics.find((d) => d.code === DIAGNOSTIC.unreadPlaceholders || d.code === DIAGNOSTIC.mixedPlaceholders || d.code === DIAGNOSTIC.repeatedParameter);
  if (unread) throw new Error(`${path}: ${unread.message}`);
  let params: string[];
  if (session.project.dialect === "mysql") {
    // A MySQL session reads its strings by its sql_mode, which the analysis asked for: its
    // placeholders are the ones the server finds.
    params = analysis.params.map((p) => p.name);
  } else {
    try {
      params = (await session.lex(sql)).params;
    } catch (e) {
      throw new Error(`${path}: ${(e as Error).message}`);
    }
  }
  // A column list that does not read, or that disagrees with the table its insert writes, fails
  // the build too.
  const descriptor = analysis.diagnostics.find((d) => d.code === DIAGNOSTIC.columnList || d.code === DIAGNOSTIC.columnListTable);
  if (descriptor) throw new Error(`${path}: ${descriptor.message}`);
  // So does a statement the database refuses (a misspelled column, a dropped table), at its line
  // and column, as the type check reports it: its module would run a statement nothing typed, with
  // no columns and any number of rows.
  const refused = analysis.diagnostics.find((d) => d.code === DIAGNOSTIC.refused);
  if (refused) throw new Error(`${path}:${place(sql, refused.start)}: ${refused.message}`);
  const dialect = session.project.dialect;
  // Each placeholder's place in the compiled statement, where a driver takes the statement in
  // pieces or with its values written in (src/execute.ts, `QueryModule`); SQLite has no such driver.
  const marks = dialect === "sqlite" ? undefined : await marksOf(analysis, trimmedStatement(analysis.rewritten.sql), dialect, (region) => session.lex(region));
  const input: ModuleInput = { analysis, raw: sql, params, marks, source: sourceName(path), dialect, codecs: session.project.codecs, sqlcommenter: session.project.sqlcommenter, allowNestedArrays: session.project.allowNestedArrays };
  // The libraries a codec builds values with are imported from the app's own install beside the
  // `.sql` file.
  for (const library of moduleLibraries(input)) {
    const missing = libraryProblem(library, dirname(path));
    if (missing) throw new Error(`${path}: ${missing}`);
  }
  return { input, session, entry: outcome.entry };
}

/**
 * What a statement's module is written from: its analysis, each domain written as its base type;
 * the statement as written and its parameters' names; each placeholder's place in the compiled
 * statement (`marksOf`); the file it came from, relative to the project's root; and the project's
 * settings that reach the module.
 */
export type ModuleInput = {
  analysis: Analysis;
  raw: string;
  params: readonly string[];
  marks: readonly (readonly [number, number])[] | undefined;
  source: string;
  dialect: Dialect;
  codecs: Codecs;
  sqlcommenter: boolean;
  allowNestedArrays: boolean;
};

/**
 * Each parameter's type, for the drivers that bind an array or a JSON value wrong, and whether it
 * takes null, which the run time refuses for one that does not (src/execute.ts). A parameter that
 * holds a family the project's `js.codecs` gives another type carries where it does, its plan,
 * which the run time binds by (src/codecs.ts).
 */
function moduleInputs({ analysis, dialect, codecs }: ModuleInput) {
  return Object.fromEntries(
    analysis.params.map((p) => {
      const codec = planOf(p.type, dialect, codecs, true);
      return [
        p.name,
        { type: p.type, ...(p.schema === undefined ? {} : { schema: p.schema }), ...(p.cast === undefined ? {} : { cast: p.cast }), ...(p.members === undefined ? {} : { members: p.members }), ...(p.delimiter === undefined ? {} : { delimiter: p.delimiter }), nullable: p.nullable === true, ...(codec === undefined ? {} : { codec }) },
      ];
    }),
  );
}

/** The columns that hold a family a codec gives another type, by name, as a row is keyed, each with where in its value the codecs stand. */
function moduleCodecs({ analysis, dialect, codecs }: ModuleInput): Record<string, Plan> {
  return Object.fromEntries(analysis.columns.flatMap((c) => {
    const plan = planOf(c.type, dialect, codecs);
    return plan === undefined ? [] : [[c.name, plan]];
  }));
}

/** The libraries a codec builds the statement's values with, only where it holds a value of one, which the module imports. */
export function moduleLibraries(m: ModuleInput): Set<Library> {
  const libraries = new Set<Library>();
  for (const plan of [...Object.values(moduleCodecs(m)), ...Object.values(moduleInputs(m)).map((i) => i.codec)]) if (plan !== undefined) planLibraries(plan, libraries);
  return libraries;
}

/** The text of a statement's module: its imports, its exports, and the definition of its query, field by field. */
export function moduleCode(m: ModuleInput): string {
  const { checked, imports } = moduleImports(m);
  const define = DEFINE[m.dialect];
  return [
    `import { ${define}${checked ? ", checks" : ""} } from "reylo";`,
    ...imports,
    // `sql` is the compiled statement and `raw` the file as written. The comment naming the file
    // (src/sqlcommenter.ts) is added where the statement is sent, so `sql` leaves it out.
    `export const sql = ${JSON.stringify(compiledText(m))};`,
    `export const raw = ${JSON.stringify(m.raw)};`,
    `export const params = ${JSON.stringify(m.params)};`,
    `export default ${define}(${moduleDefinition(m, { raw: "raw", sql: "sql", params: "params", checks: "checks" })});`,
    ``,
  ].join("\n");
}

/** The compiled statement without the file's trailing blank lines: the module's `sql`. The comment naming the file is added where the statement is sent (src/execute.ts, `sentText`). */
export function compiledText(m: ModuleInput): string {
  return trimmedStatement(m.analysis.rewritten.sql);
}

/** A statement without its trailing whitespace, but for one character after a `/*` it would leave last: SQLite reads a block comment that no `*` and `/` close as a comment to the end of the text when anything follows its `/*`, and a bare `/*` as two operators, which it refuses. The whitespace is ASCII's alone: a no-break or an ideographic space a statement ends with is part of its last name on Postgres, MySQL and SQLite (`as x\u00a0` names a column so), which `trimEnd()` would rename. */
export function trimmedStatement(sql: string): string {
  const trimmed = sql.replace(/[ \t\n\r\f\v]+$/, "");
  return trimmed.endsWith("/*") && trimmed.length < sql.length ? sql.slice(0, trimmed.length + 1) : trimmed;
}

/**
 * What a statement's module imports besides its dialect's define function: whether it needs
 * reylo's `checks`, and each library a codec builds its values with, by the names the module's
 * definition calls them (`ReyloDecimal`, `ReyloIpAddress`).
 */
export function moduleImports(m: ModuleInput): { checked: boolean; imports: string[] } {
  const { analysis } = m;
  // A decimal library's class by a default import, which every version gives as its class, and
  // ip-address whole.
  const libraries = moduleLibraries(m);
  const library = [...libraries].find((l) => l !== "ip-address");
  const imports = [...(library === undefined ? [] : [`import ReyloDecimal from ${JSON.stringify(library)};`]), ...(libraries.has("ip-address") ? [`import * as ReyloIpAddress from "ip-address";`] : [])];
  // A typed JSON, structured or cast parameter is checked with reylo's `checks`, imported only where
  // the statement has one, so that an app none of whose statements has one bundles none of them
  // (src/checks.ts). A non-null JSON parameter whose type alone carries a schema needs them too,
  // to learn whether that schema allows null.
  const checked = analysis.params.some((p) => p.schema !== undefined || p.cast !== undefined || p.members !== undefined || (p.type.kind === "json" && p.type.schema !== undefined && !p.nullable));
  return { checked, imports };
}

/**
 * The object a statement's module passes its dialect's define function, field by field, as source
 * text. `names` are the sources of the values a module holds apart from the definition: the
 * statement as written (`raw`), the compiled one (`sql`), the parameters' names and reylo's
 * `checks`. A field whose source is its own name is written as a shorthand. `multiline` puts each
 * field on a line of its own, for a file that is read and diffed.
 */
export function moduleDefinition(m: ModuleInput, names: { raw: string; sql: string; params: string; checks: string }, multiline = false): string {
  const { analysis, dialect } = m;
  const inputs = moduleInputs(m);
  // Each output column's name and kind, from the table the declared types come from: what the run
  // time gives each value (src/values.ts); and an array's delimiter where it is not a comma.
  const columns = analysis.columns.map((c) => {
    const kind = columnKind(c.type, dialect);
    // A ClickHouse column's type as the server names it stands where a delimiter would, which it never has.
    const beside = c.delimiter ?? c.described;
    return beside === undefined ? [c.name, kind] : [c.name, kind, beside];
  });
  // Where each Postgres JSON column holds a float or a decimal that may be NaN or an infinity,
  // which the run time gives as that number, or a Postgres array it holds to one level (src/render.ts,
  // `jsonNumbers`), by position beside `columns`: two columns may share a name, each with its own
  // type. Under `allow_nested_arrays: true` a Postgres array nests there too, held to no depth.
  const nonFinite = dialect === "postgres" ? (m.allowNestedArrays ? analysisWithNestedArrays(analysis) : analysis).columns.map((c) => jsonNumbers(c.type) ?? null) : [];
  const codecs = moduleCodecs(m);
  const libraries = moduleLibraries(m);
  const library = [...libraries].find((l) => l !== "ip-address");
  const handed = [...(library === undefined ? [] : ["decimal: ReyloDecimal"]), ...(libraries.has("ip-address") ? ["inet: ReyloIpAddress"] : [])];
  const { checked } = moduleImports(m);
  const text = compiledText(m);
  // A structured parameter's lists, at JavaScript's offsets into the statement (the engine's are
  // UTF-8 bytes).
  const structured = (analysis.structured ?? []).map((s) => ({ name: s.name, lists: s.lists.map((l) => ({ start: utf16(text, l.start), end: utf16(text, l.end), entries: l.entries })) }));
  // The definition's fields in order, each its name and the source of its value: one whose value
  // is undefined is left out, and one whose value is its own name is written as a shorthand.
  // `sql` is the statement as written and `text` the compiled one, the module's `raw` and `sql`.
  const fields: (readonly [name: string, value: string | undefined])[] = [
    ["sql", names.raw],
    ["params", names.params],
    ["cardinality", JSON.stringify(analysis.cardinality === "at_most_one" ? "maybeOne" : analysis.cardinality)],
    ["readOnly", analysis.readOnly ? "true" : undefined],
    ["value", analysis.value ? "true" : undefined],
    ["text", names.sql],
    ["order", JSON.stringify(analysis.rewritten.order)],
    ["marks", m.marks && JSON.stringify(m.marks)],
    ["sqlMode", analysis.sqlMode && JSON.stringify(analysis.sqlMode)],
    ["inputs", literal(inputs)],
    ["columns", literal(columns)],
    ["codecs", Object.keys(codecs).length ? literal(codecs) : undefined],
    ["libraries", handed.length ? `{ ${handed.join(", ")} }` : undefined],
    ["row", columns.length ? rowSource(columns.map(([name]) => name as string)) : undefined],
    ["nonFinite", nonFinite.some((at) => at !== null) ? literal(nonFinite) : undefined],
    ["key", analysis.key && JSON.stringify(analysis.key)],
    ["ordered", analysis.ordered ? "true" : undefined],
    ["structured", structured.length ? literal(structured) : undefined],
    ["source", JSON.stringify(m.source)],
    ["sqlcommenter", m.sqlcommenter ? "true" : undefined],
    ["nestedArrays", m.allowNestedArrays && dialect === "postgres" ? "true" : undefined],
    ["jsonSchema", literal(querySchemas(m.allowNestedArrays ? analysisWithNestedArrays(analysis) : analysis, dialect, m.codecs))],
    ["checks", checked ? names.checks : undefined],
  ];
  const written = fields.flatMap(([name, value]) => (value === undefined ? [] : [value === name ? name : `${name}: ${value}`]));
  return multiline ? `{\n${written.map((f) => `  ${f},\n`).join("")}}` : `{ ${written.join(", ")} }`;
}

/** The define function from "reylo" a module of each dialect calls (src/index.ts). */
export const DEFINE: Record<Dialect, string> = { postgres: "definePostgresQuery", mysql: "defineMysqlQuery", sqlite: "defineSqliteQuery", clickhouse: "defineClickhouseQuery" };

/**
 * Each placeholder's place in `text`, the compiled statement, at JavaScript's offsets, in text
 * order. The engine's spans say where it wrote each one; where it wrote more than the placeholder
 * (a cast, a JSON or vector parameter, a structured parameter's expansion), the span covers all
 * of it, and the placeholder is the one the engine's lexer finds in that text, which it wrote
 * itself. The lexer reads MySQL text as the default `sql_mode` does, so where the statement was
 * read under another mode, a wrap whose own strings that mode reads otherwise (a double quote, a
 * backslash) gives no marks, rather than a guess.
 */
export async function marksOf(analysis: Analysis, text: string, dialect: Dialect, lex: (sql: string) => Promise<Lexed>): Promise<[number, number][] | undefined> {
  const bytes = Buffer.from(analysis.rewritten.sql, "utf8");
  const mode = analysis.sqlMode;
  const otherMode = mode !== undefined && (!mode.backslashEscapes || mode.ansiQuotes);
  const marks: [number, number][] = [];
  for (const [, , start, end] of analysis.rewritten.spans) {
    const written = bytes.subarray(start, end);
    const region = written.toString("utf8");
    // A ClickHouse placeholder is all the engine wrote there, `{name:Type}`, whose type the run time
    // reads at its mark (src/decode-clickhouse.ts).
    if (dialect === "clickhouse" ? region.startsWith("{") && region.endsWith("}") : dialect === "mysql" ? region === "?" : /^\$[0-9]+$/.test(region)) {
      marks.push([utf16(text, start), utf16(text, end)]);
      continue;
    }
    let lexed: Lexed;
    try {
      lexed = await lex(region);
    } catch {
      return undefined;
    }
    const [p, ...more] = lexed.placeholders;
    if (p === undefined || more.length || !/^[0-9]+$/.test(p.name)) return undefined;
    if (otherMode && lexed.opaque.some(([s, e]) => written[s] === 0x22 || written.subarray(s, e).includes(0x5c))) return undefined;
    marks.push([utf16(text, start + p.start), utf16(text, start + p.end)]);
  }
  return marks;
}

/**
 * A JSON value as the module's source writes it: its JSON, a JavaScript literal of the same value,
 * but where a key is `__proto__`, which a literal takes as the object's prototype (a column of that
 * name, a key of a schema), the JSON read by `JSON.parse`, which keeps it a key.
 */
function literal(value: unknown): string {
  const text = JSON.stringify(value);
  return text.includes('"__proto__"') ? `JSON.parse(${JSON.stringify(text)})` : text;
}

/** The line and column, from 1, of a UTF-8 byte offset into `text`. */
function place(text: string, bytes: number): string {
  const before = text.slice(0, utf16(text, bytes));
  return `${before.split("\n").length}:${before.length - before.lastIndexOf("\n")}`;
}

/** The JavaScript offset into `text` of a UTF-8 byte offset. */
function utf16(text: string, bytes: number): number {
  return Buffer.from(text, "utf8").subarray(0, bytes).toString("utf8").length;
}
