// Reylo decodes every value of its own MySQL queries itself, on the app's own pool, connection or
// transaction (the representation is src/values.ts). Each driver but
// Bun.sql takes its decoding choices per query, laid over the app's without changing them
// (wiki/driver-decoding/spike/mysql.md):
//
// - mysql2 stays on `execute`, the binary protocol, unless the server refuses prepared statements:
//   then Reylo writes the values into the text itself and runs `query` (below, `mysql2Query`). Its
//   per-query options make a BIGINT and a date the server's text, and Reylo's per-query `typeCast`
//   reads a DECIMAL and a JSON value as their text, which the app's `decimalNumbers` and
//   `jsonStrings` (read from the app's config only) would otherwise decide; it runs only where a
//   statement or the app's settings need it, since mysql2 calls it for every value. A BIGINT's
//   exact text becomes a BigInt, by the result's own column type, and a date's dropped zero
//   fraction comes back from the result's fields. `decimalNumbers` is never passed per query:
//   mysql2 keys its row-parser cache, shared by every pool in the process, by the per-query value
//   and builds the parser from the app's, so one query would poison another pool's parser.
// - mariadb takes every option per query, both ways: a BIGINT is its own BigInt (mariadb 2's too), and
//   `metaAsArray: false` keeps the rows an array of rows. A SET comes back an array, which no
//   option prevents: Reylo joins it.
// - PlanetScale takes a `cast` per query, which replaces the app's: Reylo's is the driver's own
//   default, with a BIGINT a BigInt and a DATETIME's `T`.
// - Bun.sql takes nothing per query: its values are converted (src/decode.ts, `bunRows`).
//
// MySQL has no boolean type: a TINYINT(1), a BIT(1) and a comparison are integers to every driver,
// and a column whose kind is `bool` (src/values.ts) is made a boolean after the driver, a value
// other than 0 or 1 refused (`truth`).
import { lastOfEach, namesInResult, rowOf } from "./row.ts";
import { bigInteger, coerce, DecodeError, describe, ParameterError, truth, utf8, withFraction, type Columns, type Kind } from "./values.ts";
import { bunRows } from "./decode.ts";
import { readMysqlError } from "./query-error.ts";
import { literalsAtMarks, takesFloat, type AnyClient as Client, type Bound, type Marks, type QueryModule, type Runtime } from "./execute.ts";
import type { SqlMode } from "./engine-core.ts";

type AnyClient = Record<string, any>;

// ---------------------------------------------------------------------------------------------
// mysql2

/**
 * Options mysql2 ORs with the app's (a BIGINT and a date as text) and the row's shape, which the query's own replace. Rows are read by position: mysql2's key is the name the executed statement's metadata gives, with a trailing space where MySQL keeps one (`select true` is `true `, as is `current_date` and an unaliased `case … end`), and building an object under the name `__proto__` fails in the driver and closes the connection.
 */
const MYSQL2_OPTIONS = { supportBigNumbers: true, bigNumberStrings: true, dateStrings: true, rowsAsArray: true, nestTables: false };

type Mysql2Field = { type: string; string(encoding?: string): string | null; buffer(): Uint8Array | null };

/**
 * Reylo's typeCast: a DECIMAL as its text and a JSON value parsed from its bytes (both are
 * length-coded in the binary protocol too, so they read whole), JSON here because mysql2's own
 * parser turns an integer past 2^53 inside a document into a string under `supportBigNumbers`; a
 * GEOMETRY as its bytes; every other type mysql2's own reading with the options above.
 */
function mysql2Cast(f: Mysql2Field, next: () => unknown): unknown {
  switch (f.type) {
    case "NEWDECIMAL":
    case "DECIMAL":
      return f.string("ascii");
    case "JSON": {
      const bytes = f.buffer();
      return bytes === null ? null : JSON.parse(utf8(bytes));
    }
    case "GEOMETRY":
      return f.buffer();
    default:
      return next();
  }
}

/**
 * Reylo's typeCast for the text protocol, where every value is its text: a DOUBLE and a FLOAT read
 * by Number(), since mysql2 reads a DOUBLE's text digit by digit and misses the nearest double for
 * some of them, and a VECTOR as its bytes, since mysql2's text parser reads a NULL VECTOR as [].
 */
function mysql2TextCast(f: Mysql2Field, next: () => unknown): unknown {
  if (f.type === "DOUBLE" || f.type === "FLOAT") {
    const s = f.string("ascii");
    return s === null ? null : Number(s);
  }
  if (f.type === "VECTOR") return f.buffer();
  return mysql2Cast(f, next);
}

/** A column's protocol type and fraction digits, as mysql2's fields give them. */
type Mysql2Column = { name: string; columnType: number; decimals: number };
const DATETIME = 12, TIMESTAMP = 7, TIME = 11;

/**
 * A DATETIME or TIMESTAMP with `T`, and the fraction digits the binary protocol drops when they are
 * all zero (mysql2 writes `09:31:00` for a DATETIME(3) of .000, and TIME's trailing zeros never),
 * from the column's own count.
 */
function wallClock(s: string, decimals: number, type: number): string {
  const text = withFraction(s, decimals);
  return type === TIME ? text : text.replace(" ", "T");
}

/** The app's settings, where mysql2 shows them: a promise Pool's, Connection's or PoolConnection's. */
type Mysql2Config = { typeCast?: unknown; decimalNumbers?: boolean; jsonStrings?: boolean; queryFormat?: unknown; charsetNumber?: number };
const configOf = (m: AnyClient): Mysql2Config | undefined => m.pool?.config?.connectionConfig ?? m.connection?.config;

/**
 * Whether the statement needs Reylo's typeCast, which mysql2 calls for every value of the binary
 * protocol (about 70 ns each): for a JSON or GEOMETRY column, whenever the columns are not known,
 * and where the app's settings, which a query cannot turn off, would decide a DECIMAL or a JSON value
 * or replace mysql2's own reading (or cannot be read).
 */
function needsCast(m: AnyClient, columns: Columns | undefined): boolean {
  if (!columns || columns.some(([, kind]) => kind === "unknown" || kind.split("|").some((k) => k === "geometry" || k === "json" || k.startsWith("json")))) return true;
  const config = configOf(m);
  return config === undefined || typeof config.typeCast === "function" || config.decimalNumbers === true || config.jsonStrings === true;
}

/** mysql2's protocol type of a BIGINT (MYSQL_TYPE_LONGLONG). */
const LONGLONG = 8;

/** What a column's values need after a driver: a truth value made a boolean, a BIGINT a BigInt. */
type Fix = (v: unknown, name: string) => unknown;
const toBool: Fix = truth;
// A BIGINT's text is always its digits: BigInt reads it straight.
const toBigInt: Fix = (v, name) => (typeof v === "string" ? BigInt(v) : typeof v === "number" || typeof v === "bigint" ? bigInteger(v, name) : v);

// A VECTOR is MySQL's one array: mysql2 decodes its floats, mariadb and PlanetScale give its bytes.
const toFloats: Fix = (v, name) => coerce("float[]", v, name);
// A SET is a Set of its members: mysql2 and PlanetScale give its text, mariadb an array.
const toSet: Fix = (v, name) => coerce("set", v, name);
// An ENUM holds one of its labels, or the `''` MySQL stores for any other value, which is refused.
const toEnum: Fix = (v, name) => coerce("enum", v, name);

/**
 * The fix a column of `kind` needs, if any: `bool` a boolean, `int8` (a BIGINT, or an integer a
 * temporary table may widen to one) a BigInt from whatever the driver gave, `float[]` (a VECTOR)
 * its floats, `set` a Set of its members, `enum` its label.
 */
function byKind(kind: Kind | undefined): Fix | undefined {
  if (kind === "bool") return toBool;
  if (kind === "int8") return toBigInt;
  if (kind === "float[]") return toFloats;
  if (kind === "set") return toSet;
  if (kind === "enum") return toEnum;
  return undefined;
}

/** Applies each column's fix, in place: the drivers build new rows per call. */
function fixRows(rows: Record<string, unknown>[], fixes: (readonly [string, Fix])[]): void {
  if (!fixes.length) return;
  for (const row of rows) {
    for (const [name, fix] of fixes) {
      const v = row[name];
      if (v !== null && v !== undefined) row[name] = fix(v, name);
    }
  }
}

// ---------------------------------------------------------------------------------------------
// mysql2 through query(), where the server refuses prepared statements (Cloudflare Hyperdrive:
// "Hyperdrive does not currently support MySQL COM_STMT_PREPARE messages"). Reylo writes each
// value into the statement itself, at the module's marks, the places the engine found its
// placeholders, and sends the text with no values, so mysql2's escaping and placeholder scan
// decide nothing. A place is a placeholder only for a session that reads the text's strings as the
// build did: under NO_BACKSLASH_ESCAPES a backslash escapes nothing, and under ANSI_QUOTES a
// double-quoted word is a name, so where the statement's reading turns on either, the module
// carries the build's mode, and a session whose own differs is refused rather than given a value
// at a place it reads as inside a string. A string value is quoted with each quote doubled, which
// every sql_mode reads the same way, and a backslash in it is escaped as the session reads it.
// The session's mode is read on the connection the statement then runs on, rather than from the
// server's NO_BACKSLASH_ESCAPES status flag, which can be stale after a stored routine changes
// sql_mode. A value holding a character outside ASCII is refused on a connection whose charset is
// not UTF-8, and on a session that reads the text in another charset, as after SET NAMES, and so is
// the app's queryFormat, which would rewrite the statement. A run that reads the session first
// keeps its turn on the connection (inTurn), so that a run called after it cannot overtake it.

/** The pools and connections whose server refused a prepared statement: their statements go through query() from then on. */
const refusesPrepares = new WeakSet<object>();

/** The client a refusal is remembered on: the pool or connection under a promise wrapper, which mysql2's callback API makes anew for every call. */
const preparesKey = (m: AnyClient): object => m.pool ?? m.connection ?? m;

/**
 * Whether an error is the server's refusal of a prepared statement. Only the server's own message
 * is read, up to its first quote: mariadb's message adds the statement, and a server's message
 * quotes values, as "Duplicate entry '…'" does.
 */
const isPrepareRefusal = (e: unknown) => e instanceof Error && /^[^'"]*\bCOM_STMT_PREPARE\b/.test(String((e as { sqlMessage?: unknown }).sqlMessage ?? e.message));

/** The read of how a session reads a statement's text: its sql_mode, and character_set_client, the charset it reads the text in. */
const SESSION_READ = "select @@session.sql_mode, @@session.character_set_client";

/** Per connection, the runs called on it so far: settled once each has handed the driver its statement, and once each has its answer. */
type Turn = { handed: Promise<void>; done: Promise<void> };
const turns = new WeakMap<object, Turn>();

/**
 * `start` in its turn on the connection `key`, after every run called on it before; a pool, which
 * runs each statement on whichever connection is free, has no key. A run that reads the session
 * first hands the driver its statement only once the read answers, so a run called after it waits
 * until it has rather than overtaking it. A run that reads the driver's own state of the session
 * (`quiet`) waits until every run before it has its answer, since the driver takes that state from
 * each answer. With no run in the way, `start` begins at once, and the driver queues its statement
 * in the order it was called. `start` calls `handed` once the driver has its statement.
 */
async function inTurn<T>(key: object | undefined, quiet: boolean, start: (handed: () => void) => Promise<T>): Promise<T> {
  const before = key === undefined ? undefined : turns.get(key);
  let handed = () => {};
  let finished = () => {};
  if (key !== undefined) {
    const ownHanded = new Promise<void>((resolve) => (handed = resolve));
    const ownDone = new Promise<void>((resolve) => (finished = resolve));
    const turn = { handed: before ? before.handed.then(() => ownHanded) : ownHanded, done: before ? before.done.then(() => ownDone) : ownDone };
    turns.set(key, turn);
    void turn.done.then(() => turns.get(key) === turn && turns.delete(key));
  }
  try {
    if (before) await (quiet ? before.done : before.handed);
    return await start(handed);
  } finally {
    handed();
    finished();
  }
}

/** The statement just handed to the driver, `running`, once `handed` has said so. */
function handing<T>(running: Promise<T>, handed: () => void): Promise<T> {
  handed();
  return running;
}

const hexOf = (bytes: Uint8Array) => {
  let hex = "";
  for (const byte of bytes) hex += (byte < 16 ? "0" : "") + byte.toString(16);
  return hex;
};

/**
 * The collations of utf8mb4 and utf8mb3 that every mysql2 from 2.3 on encodes as UTF-8
 * (charset_encodings.js). UTF-8 writes a character outside ASCII with no ASCII byte; another
 * encoding can write one with a quote's or a backslash's byte (SJIS writes `¥` as 0x5c, and
 * Node's latin1 and ascii keep a character's low byte, so `Ŝ` is 0x5c and `ħ` is 0x27), which the
 * server would read as that character after Reylo had escaped the value. Every client charset
 * writes ASCII the same way.
 */
const UTF8_COLLATIONS: readonly (readonly [number, number])[] = [[33, 33], [45, 46], [76, 76], [83, 83], [192, 215], [223, 247], [255, 271], [273, 275], [277, 294], [296, 298], [300, 300], [303, 305]];
const isUtf8 = (charsetNumber: number) => UTF8_COLLATIONS.some(([from, to]) => charsetNumber >= from && charsetNumber <= to);

/** A string literal for a session that reads a backslash as an escape (`escapes`) or as itself. */
function stringLiteral(s: string, escapes: boolean): string {
  const quoted = s.replace(/'/g, "''");
  return `'${escapes ? quoted.replace(/\\/g, "\\\\") : quoted}'`;
}

/** The text a value is written as a string literal with, where it is one. */
const textOf = (v: unknown): string | undefined => (typeof v === "string" ? v : v !== null && typeof v === "object" && !(v instanceof Uint8Array) && !(v instanceof Date) ? JSON.stringify(v) : undefined);

/** A value as the literal the server reads as what execute() binds for it: a number that is no safe integer as a DOUBLE (`1.5e0`), never the DECIMAL `1.5` is. */
function mysqlLiteral(v: unknown, m: AnyClient, escapes: boolean): string {
  if (v === null || v === undefined) return "NULL";
  switch (typeof v) {
    case "boolean":
      return v ? "TRUE" : "FALSE";
    case "bigint":
      return String(v);
    case "number": {
      if (!Number.isFinite(v)) throw new TypeError(`reylo cannot write ${v} into a MySQL statement, which it runs through mysql2's query() on a server that refuses prepared statements: MySQL has no such number.`);
      if (Object.is(v, -0)) return "-0e0";
      if (Number.isSafeInteger(v)) return String(v);
      const s = String(v);
      return s.includes("e") ? s : `${s}e0`;
    }
    case "string":
      return stringLiteral(v, escapes);
  }
  // `_binary`, since MySQL reads two bare hex literals in a bitwise operator as numbers.
  if (v instanceof Uint8Array) return `_binary X'${hexOf(v)}'`;
  // A Date in the connection's timezone, as execute() binds one; its text holds no quote or backslash.
  if (v instanceof Date) return m.escape(v);
  return stringLiteral(JSON.stringify(v), escapes);
}

/** `text` with the placeholder at each mark replaced by its value's literal, for a session that reads a backslash in a string as an escape (`escapes`) or as itself. */
function inlineMysql(text: string, marks: Marks, values: unknown[], m: AnyClient, escapes: boolean): string {
  if (marks.length !== values.length) throw new TypeError(`reylo has ${marks.length} placeholders for ${values.length} values in this MySQL statement.`);
  let out = "";
  let at = 0;
  marks.forEach(([start, end], i) => {
    if (text.slice(start, end) !== "?") throw new TypeError(`reylo cannot write a value into this MySQL statement at ${start}, which holds ${JSON.stringify(text.slice(start, end))}, not a placeholder ?.`);
    out += text.slice(at, start) + mysqlLiteral(values[i], m, escapes);
    at = end;
  });
  return out + text.slice(at);
}

/** A session's sql_mode as the two flags that decide how it reads text. */
function readsText(sqlMode: string): SqlMode {
  const flags = sqlMode.split(",");
  return { backslashEscapes: !flags.includes("NO_BACKSLASH_ESCAPES"), ansiQuotes: flags.includes("ANSI_QUOTES") };
}

/** What a sql_mode has of the two flags that decide how it reads text. */
function modeWords(mode: SqlMode): string {
  const set = [...(mode.backslashEscapes ? [] : ["NO_BACKSLASH_ESCAPES"]), ...(mode.ansiQuotes ? ["ANSI_QUOTES"] : [])];
  const unset = [...(mode.backslashEscapes ? ["NO_BACKSLASH_ESCAPES"] : []), ...(mode.ansiQuotes ? [] : ["ANSI_QUOTES"])];
  return set.length === 2 ? `has ${set.join(" and ")}` : set.length === 0 ? `has neither ${unset.join(" nor ")}` : `has ${set[0]} and not ${unset[0]}`;
}

/** Field names mysql2's compiled parsers refuse (helpers.js, privateObjectProps), which a string written into the text names an unaliased column, MySQL taking the bytes up to a space off its start. */
const PRIVATE_NAMES = new Set(["__proto__", "__defineGetter__", "__defineSetter__", "__lookupGetter__", "__lookupSetter__"]);

/**
 * The statement through query(), its values written in at its marks. Where the module carries the
 * sql_mode it was built under, or a value holds a backslash, the session's sql_mode is read first,
 * on the connection the statement then runs on.
 */
async function mysql2Query(m: AnyClient, text: string, marks: Marks | undefined, built: SqlMode | undefined, values: unknown[], columns: Columns | undefined): Promise<[unknown, unknown]> {
  if (marks === undefined) throw new TypeError("reylo cannot run this mysql statement through mysql2 here: this server, as Cloudflare Hyperdrive does, refuses prepared statements, so Reylo writes the values into the statement, and a query defined by hand needs `marks` there, the place of each placeholder in its text, which a build step writes.");
  const config = configOf(m);
  if (typeof config?.queryFormat === "function") throw new Error("reylo cannot run a mysql statement through mysql2 here: this server, as Cloudflare Hyperdrive does, refuses prepared statements, so Reylo runs mysql2's query(), which passes every statement through the app's queryFormat. Connect without a queryFormat, or with the mariadb driver.");
  const wide = values.some((v) => /[^\x00-\x7f]/.test(textOf(v) ?? ""));
  if (config?.charsetNumber !== undefined && !isUtf8(config.charsetNumber) && wide) throw new Error("reylo cannot run a mysql statement through mysql2 here: this server, as Cloudflare Hyperdrive does, refuses prepared statements, so Reylo writes the values into the statement, and this connection's charset is not UTF-8, so a character outside ASCII in a value could reach the server as a quote or a backslash (as SJIS writes ¥). Connect with utf8mb4.");
  const proto = columns?.some(([name]) => name === "__proto__") || values.some((v) => typeof v === "string" && PRIVATE_NAMES.has(v.replace(/^[\x00-\x20]+/, ""))) ? { disableEval: true } : {};
  const options = { ...MYSQL2_OPTIONS, ...proto, namedPlaceholders: false, typeCast: mysql2TextCast };
  const pool = typeof m.getConnection === "function" && typeof m.release !== "function";
  return await inTurn(pool ? undefined : (m.connection ?? m), false, async (handed) => {
    // Without a backslash or a character outside ASCII in a value, every session reads the values alike.
    if (built === undefined && !wide && !values.some((v) => textOf(v)?.includes("\\"))) return await handing(m.query({ sql: inlineMysql(text, marks, values, m, true), ...options }), handed);
    // A promise Pool hands out a PoolConnection, so that the probe and the statement share one session.
    const pooled = pool ? await m.getConnection() : undefined;
    const conn = pooled ?? m;
    try {
      // The session's charset as well where a value holds a character outside ASCII: SET NAMES changes how the server reads the text, and mysql2 writes it as it connected.
      const [rows] = await conn.query({ sql: wide ? SESSION_READ : "select @@session.sql_mode", rowsAsArray: true, nestTables: false, namedPlaceholders: false, typeCast: true });
      const mode = readsText(String(rows[0][0]));
      if (built !== undefined && (mode.backslashEscapes !== built.backslashEscapes || mode.ansiQuotes !== built.ansiQuotes)) {
        throw new Error(`reylo cannot run this mysql statement through mysql2 here: this server, as Cloudflare Hyperdrive does, refuses prepared statements, so Reylo writes the values into the statement at its placeholders, which the build found as a session reads them whose sql_mode ${modeWords(built)}, and this session's sql_mode ${modeWords(mode)}, so it reads the statement's strings otherwise. Set the session's sql_mode as the build's was, or build against a server whose sql_mode is this one's.`);
      }
      if (wide && !String(rows[0][1]).startsWith("utf8")) {
        throw new Error(`reylo cannot run a mysql statement through mysql2 here: this server, as Cloudflare Hyperdrive does, refuses prepared statements, so Reylo writes the values into the statement, and this session reads it in ${String(rows[0][1])}, not UTF-8, as after SET NAMES, so a character outside ASCII in a value could reach the server as a quote or a backslash. Connect with utf8mb4, and leave the session's charset as it is.`);
      }
      return await handing(conn.query({ sql: inlineMysql(text, marks, values, m, mode.backslashEscapes), ...options }), handed);
    } finally {
      pooled?.release();
    }
  });
}

/** mysql2's execute(), resolving to [rows, fields], or its query() where the server refuses prepared statements. */
export async function mysql2(m: AnyClient, text: string, values: unknown[], columns: Columns | undefined, marks?: Marks, built?: SqlMode): Promise<unknown[]> {
  // mysql2's compiled parser refuses a field named `__proto__` even for an array row, and its interpreted one (`disableEval`, from 3.15) reads it.
  const proto = columns?.some(([name]) => name === "__proto__") ? { disableEval: true } : {};
  let answer: [unknown, unknown];
  if (refusesPrepares.has(preparesKey(m))) {
    answer = await mysql2Query(m, text, marks, built, values, columns);
  } else {
    try {
      answer = await m.execute({ sql: text, ...MYSQL2_OPTIONS, ...proto, typeCast: needsCast(m, columns) ? mysql2Cast : true }, values);
    } catch (e) {
      if (!isPrepareRefusal(e)) throw e;
      refusesPrepares.add(preparesKey(m));
      answer = await mysql2Query(m, text, marks, built, values, columns);
    }
  }
  const [result, fields] = answer;
  if (!Array.isArray(result)) return [];
  if (!result.length) return result;
  const described = (fields ?? []) as Mysql2Column[];
  const cols = resultColumns(columns, described.map((f) => f.name), result[0]);
  const rows = asObjects(result, cols);
  // A row keeps the last value of each name, so that column's fixes alone apply to it.
  const kept = new Set(lastOfEach(cols.map(([name], i) => [name, i] as const)).map(([, i]) => i));
  const fixes: (readonly [string, Fix])[] = [];
  // The module's kind; a BIGINT by the result's own type, which says it of an either too (`:n + 1`,
  // a BIGINT or a DOUBLE by what is bound).
  described.forEach((f, i) => {
    const fix = byKind(cols[i]![1]) ?? (f.columnType === LONGLONG ? toBigInt : undefined);
    if (fix && kept.has(i)) fixes.push([cols[i]![0], fix]);
  });
  // A DATETIME's, TIMESTAMP's and TIME's text, padded to the column's own fraction digits.
  described.forEach((f, i) => {
    if (kept.has(i) && (f.columnType === DATETIME || f.columnType === TIMESTAMP || (f.columnType === TIME && f.decimals > 0))) {
      const decimals = f.decimals > 6 ? 0 : f.decimals, type = f.columnType;
      fixes.push([cols[i]![0], (v) => (typeof v === "string" ? wallClock(v, decimals, type) : v)]);
    }
  });
  fixRows(rows, fixes);
  return rows;
}

// ---------------------------------------------------------------------------------------------
// mariadb

/**
 * Every decoding option, each laid over the app's for this query (mariadb's parser.js:
 * Object.assign(Object.create(connOpts), opts)). mariadb 3: with `bigIntAsNumber` and
 * `supportBigNumbers` off a BIGINT is its own BigInt, and with `decimalAsNumber` off too a DECIMAL
 * its text (`bigNumberStrings` matters only beside `supportBigNumbers`). mariadb 2, which has none
 * of the first two: `supportBigInt` makes a BIGINT a BigInt and `bigNumberStrings` a DECIMAL its
 * text, where it would be a lossy number.
 */
const MARIADB_OPTIONS = {
  bigIntAsNumber: false,
  decimalAsNumber: false,
  supportBigNumbers: false,
  bigNumberStrings: true,
  supportBigInt: true,
  checkNumberRange: false,
  dateStrings: true,
  jsonStrings: false,
  autoJsonMap: true,
  bitOneIsBoolean: false,
  rowsAsArray: false,
  nestTables: false,
  metaAsArray: false,
  typeCast: undefined,
  // Reylo passes the values by position, and execute() reads only `:name` under the app's namedPlaceholders.
  namedPlaceholders: false,
};

/** A GEOMETRY's bytes, where mariadb would give GeoJSON; only for a statement whose columns hold one, since a typeCast sends every value through mariadb's slower path. */
const mariadbGeometry = (column: { type: string; buffer(): unknown }, next: () => unknown) => (column.type === "GEOMETRY" ? column.buffer() : next());

type MariadbColumn = { type?: string; columnType?: number; name?: string | (() => string); isSet?: () => boolean; scale?: number };

// mariadb writes each value into the statement's text itself, escaping it as its escape() does:
// a quote doubled, which every session reads alike, and a backslash as it is, or each of them after
// a backslash. Either way the session must read a backslash as the driver took it. The two hazards
// of mysql2's query() hold here too: the server's NO_BACKSLASH_ESCAPES status flag, which the
// driver may go by, can be stale after a stored routine changes sql_mode, and a charset other than
// UTF-8 can write a character outside ASCII with a quote's or a backslash's byte. A third is this
// driver's alone: in big5, cp932, gb18030, gbk and sjis a character's second byte may be a
// backslash's, so a session in one of them can read a byte of 0x80 or above and the backslash the
// driver writes after it as one character, and the quote or backslash that backslash escaped stands
// bare. Some versions of the driver write such bytes as the session reads them and some do not, and
// nothing on the client tells which. So where a value holds a backslash, a quote the driver writes
// after one, or a character outside ASCII, the session's sql_mode and character_set_client, the
// charset it reads the statement in, are read first, on the connection the statement then runs on,
// and a value the session would read otherwise than the driver wrote it is refused: on those five
// charsets, wherever the driver writes backslashes, that is bytes with a quote or a backslash after
// a byte of 0x80 or above. A driver from before mariadb 3.2.5, 3.3.4, 3.4.7 and 3.5.4 writes
// backslashes on every session, so where it was told the session is in NO_BACKSLASH_ESCAPES a value
// holding a double quote or a control character is read for too. And the driver finds the
// statement's placeholders with a scan of its own, which reads the text as the default sql_mode
// does, so a statement whose build read its strings otherwise is refused (scansAsBuilt), and one
// whose build read them so runs only on a session that reads them so too.

const QUOTE = 0x27, BACKSLASH = 0x5c;

/** The client charsets of MySQL in which a character's second byte may be a backslash's, 0x5c. */
const BACKSLASH_TRAILS = new Set(["big5", "cp932", "gb18030", "gbk", "sjis"]);

/** Whether a value is bytes with one of `codes` directly after a byte of 0x80 or above. */
const highBefore = (v: unknown, codes: readonly number[]) => v instanceof Uint8Array && v.some((byte, i) => byte >= 0x80 && codes.includes(v[i + 1]!));

/** Whether a value is bytes with a character the driver writes a backslash before (a quote, a backslash, a double quote or NUL) directly after a byte of 0x80 or above, a byte those charsets may read with that backslash as one character. */
const leadsAnEscape = (v: unknown) => highBefore(v, [QUOTE, BACKSLASH, 0x22, 0x00]);

/** How the charsets whose second byte may be a backslash's write 中, which the read has the driver write, so that its bytes show the charset the driver writes text in. */
const FAMILY_WRITING: Readonly<Record<string, string>> = { E4B8AD: "utf8", A4A4: "big5", D6D0: "gbk", "9286": "sjis" };

/** A charset as the family that writes 中 alike: UTF-8, big5, gbk and gb18030, sjis and cp932, or another. */
const familyOf = (charset: string) => (charset.startsWith("utf8") ? "utf8" : charset === "big5" ? "big5" : charset === "gbk" || charset === "gb18030" ? "gbk" : charset === "sjis" || charset === "cp932" ? "sjis" : "other");

/** Whether a value holds the character `code`: in its text, or in its bytes, which mariadb escapes as it does a string's. */
const holds = (v: unknown, code: number) => (v instanceof Uint8Array ? v.includes(code) : (textOf(v)?.includes(String.fromCharCode(code)) ?? false));

/** A mariadb call's answer: its promise API returns a promise, its callback API answers through the callback. */
function answer<T>(call: (done: (error: Error | null, value?: T) => void) => unknown): Promise<T> {
  return new Promise((resolve, reject) => {
    const returned = call((error, value) => (error ? reject(error) : resolve(value as T)));
    if (typeof (returned as PromiseLike<T> | undefined)?.then === "function") (returned as PromiseLike<T>).then(resolve, reject);
  });
}

/** The characters other than a quote and a backslash that mariadb, writing backslashes, writes one before: a double quote, NUL, and before 3.5 the control characters too. */
const OTHER_ESCAPED = [0x22, 0x00, 0x08, 0x09, 0x0a, 0x0d, 0x1a];

/** The server's status flag NO_BACKSLASH_ESCAPES, which the driver keeps from each reply in `info.status`. */
const STATUS_NO_BACKSLASH_ESCAPES = 1 << 9;

/**
 * How mariadb and PlanetScale find the placeholders in a statement's text, each with a scan of its
 * own: as a session whose sql_mode has neither NO_BACKSLASH_ESCAPES nor ANSI_QUOTES reads it, a
 * backslash in a string escaping the character after it and a double quote opening a string.
 */
const SCANS: SqlMode = { backslashEscapes: true, ansiQuotes: false };

const sameReading = (a: SqlMode, b: SqlMode) => a.backslashEscapes === b.backslashEscapes && a.ansiQuotes === b.ansiQuotes;

/**
 * A statement whose strings each sql_mode reads otherwise carries the mode its build read it under
 * (`built`), and its placeholders are the ones that reading found. A driver whose scan reads it
 * otherwise would write a value at a place the build read as inside a string, where the value's
 * own quote ends that string and the rest of the value is read as SQL, so it is refused.
 */
function scansAsBuilt(driver: string, built: SqlMode | undefined) {
  if (built === undefined || sameReading(built, SCANS)) return;
  throw new Error(`reylo cannot run this mysql statement through ${driver}, which finds the placeholders in its text as a session reads them whose sql_mode ${modeWords(SCANS)}, and the build found them as a session reads them whose sql_mode ${modeWords(built)}, which reads the statement's strings otherwise, so ${driver} could write a value where the build found a string. Build against a server whose sql_mode ${modeWords(SCANS)}, or run the statement through mysql2.`);
}

/**
 * `run` on a session that reads each value as mariadb writes it, and the statement's strings as its
 * build did (`built`); a client without escape() is taken to write a quote after a backslash. A Pool
 * hands out a PoolConnection first, so that the read and the statement share one session.
 */
async function asWritten<T>(c: AnyClient, values: unknown[], built: SqlMode | undefined, run: (conn: AnyClient) => Promise<T>): Promise<T> {
  const wide = values.some((v) => /[^\x00-\x7f]/.test(textOf(v) ?? ""));
  const backslash = values.some((v) => holds(v, BACKSLASH));
  const quote = values.some((v) => holds(v, QUOTE));
  const other = values.some((v) => OTHER_ESCAPED.some((code) => holds(v, code)));
  // Bytes whose high byte a charset may read with the byte after it as one character.
  const trailed = values.some(leadsAnEscape);
  const plain = !wide && !backslash && !quote && !other && !trailed && built === undefined;
  const pool = typeof c.getConnection === "function" && typeof c.release !== "function";
  return await inTurn(pool ? undefined : c, !plain, async (handed) => {
    // Where the session needs no read, the statement goes to the driver before anything is awaited,
    // so that a call made on the connection next, its own rollback() too, follows it.
    if (plain || (!pool && !mustRead(c))) return await handing(run(c), handed);
    const pooled = pool ? await answer<AnyClient>((done) => c.getConnection(done)) : undefined;
    const conn = pooled ?? c;
    try {
      if (mustRead(conn)) await readFor(conn);
      return await handing(run(conn), handed);
    } finally {
      if (pooled) await answer((done) => pooled.release(done));
    }
  });

  /**
   * How the driver writes a quote on this connection, after a backslash or doubled (which every
   * session reads alike), and so whether the session must read a backslash as the driver does. The
   * driver goes by the flag of the last answer it had, so this is read with no run of Reylo's still
   * waiting on the connection (inTurn), and again after the session's read, the last answer before
   * the statement.
   */
  function writes(conn: AnyClient) {
    const escapes = typeof conn.escape !== "function" || conn.escape("'") !== "''''";
    // A driver that writes backslashes on a session it was told is in NO_BACKSLASH_ESCAPES goes by
    // no flag (mariadb before 3.2.5, 3.3.4, 3.4.7 and 3.5.4), and such a session would keep the
    // backslash it writes before a double quote or a control character: `say "hi"` stored as
    // `say \"hi\"`. A flag left stale by a stored routine that took NO_BACKSLASH_ESCAPES out is not
    // read for here: that would cost a statement for nearly every JSON value.
    const told = ((Number(conn.info?.status) || 0) & STATUS_NO_BACKSLASH_ESCAPES) !== 0;
    return { escapes, slashed: backslash || (escapes && (quote || (other && told))) };
  }

  /** Whether the session must be read before the statement goes to the driver. */
  function mustRead(conn: AnyClient) {
    return wide || trailed || writes(conn).slashed || built !== undefined;
  }

  /** The session's read, which refuses a value the session would read otherwise than the driver writes it. */
  async function readFor(conn: AnyClient): Promise<void> {
    // Where a value holds a character outside ASCII or such bytes, the read also has the driver
    // write 中, whose bytes show the charset it writes text in: the session's charset says how the
    // server reads text, not how the driver writes it, which a server that reports no change of
    // charset leaves as it was.
    const encoded = wide || trailed;
    const [[mode, charset, hex]] = await answer<[[unknown, unknown, unknown?]]>((done) => conn.query({ sql: encoded ? `${SESSION_READ}, hex(_binary ?)` : SESSION_READ, ...MARIADB_OPTIONS, rowsAsArray: true }, encoded ? ["中"] : [], done));
    const { escapes, slashed } = writes(conn);
    const reading = readsText(String(mode));
    const reads = String(charset);
    const writing = FAMILY_WRITING[String(hex).toUpperCase()] ?? "other";
    if (built !== undefined && !sameReading(reading, built)) {
      throw new Error(`reylo cannot run this mysql statement through mariadb: the build found its placeholders as a session reads them whose sql_mode ${modeWords(built)}, and this session's sql_mode ${modeWords(reading)}, so it reads the statement's strings otherwise. Set the session's sql_mode as the build's was.`);
    }
    if (slashed && reading.backslashEscapes !== escapes) {
      throw new Error(`reylo cannot run this mysql statement through mariadb: mariadb writes each value into the statement ${escapes ? "with a backslash before each quote, double quote and backslash, and this session's sql_mode, with NO_BACKSLASH_ESCAPES, reads a backslash as itself" : "with each quote doubled and each backslash as it is, and this session's sql_mode, without NO_BACKSLASH_ESCAPES, reads a backslash as an escape"}, so it would read a value holding ${escapes ? "a quote, a double quote, a backslash or a control character otherwise than the driver wrote it. Take NO_BACKSLASH_ESCAPES out of the session's sql_mode, or use mariadb 3.2.5, 3.3.4, 3.4.7, 3.5.4 or later, which write each value as such a session reads it, or, if a stored routine set sql_mode, set it again after the call (set session sql_mode = @@session.sql_mode), which brings the server's report of it up to date." : "a backslash otherwise than the driver wrote it. Set the session's sql_mode again (set session sql_mode = @@session.sql_mode), which brings the server's report of it up to date, as after a stored routine that set it."}`);
    }
    if (wide && !reads.startsWith("utf8")) {
      throw new Error(`reylo cannot run a mysql statement through mariadb with a value holding a character outside ASCII: mariadb writes each value into the statement, and this connection's charset, ${reads}, is not UTF-8, so such a character could reach the server as a quote or a backslash (as SJIS writes ¥). Connect with utf8mb4.`);
    }
    if (wide && writing !== "utf8") {
      throw new Error(`reylo cannot run a mysql statement through mariadb with a value holding a character outside ASCII: mariadb writes each value into the statement, and on this connection it writes text in a charset that is not UTF-8 while the session reads it as UTF-8, as after SET NAMES on a server that reports no change of charset, so such a character could reach the server as a quote or a backslash (as SJIS writes ソ). Connect with utf8mb4.`);
    }
    if (escapes && BACKSLASH_TRAILS.has(reads) && trailed) {
      throw new Error(`reylo cannot run a mysql statement through mariadb with bytes holding a quote, a double quote, a backslash or NUL directly after a byte of 0x80 or above: mariadb writes each value into the statement with a backslash before each of them, and this connection's charset, ${reads}, can read such a byte and that backslash as one character, so the character after them could end the value's literal or escape its closing quote, or be stored with the byte altered. Connect with utf8mb4.`);
    }
    // A driver that writes in one of those charsets leaves a backslash that follows such a byte as
    // it is, the second byte of one character, which a session in another charset reads as an escape.
    if (writing !== "other" && writing !== "utf8" && writing !== familyOf(reads) && values.some((v) => highBefore(v, [BACKSLASH]))) {
      throw new Error(`reylo cannot run a mysql statement through mariadb with bytes holding a backslash directly after a byte of 0x80 or above: on this connection mariadb writes text in ${writing}, which reads such a byte and the backslash as one character and so leaves the backslash as it is, and the session reads the text in ${reads}, which reads that backslash as an escape. Connect with utf8mb4.`);
    }
  }
}

/** A TIME's text with its column's `digits` of fraction, where execute() writes six, or none where they are all zero. */
function timeOfScale(text: string, digits: number): string {
  const dot = text.indexOf(".");
  if (dot < 0) return withFraction(text, digits);
  return digits <= 0 ? text.slice(0, dot) : withFraction(text.slice(0, dot + 1 + digits), digits);
}

/**
 * mariadb's execute(), which prepares the statement and binds each value, or its query(), which
 * writes each value into the statement's text, on its promise or its callback API, with Reylo's
 * options. mariadb 3 runs each statement through execute(), so that the server reads every value as
 * a value, whatever the session's sql_mode or charset, and no read of the session stands between a
 * run and its statement. mariadb 2 has no execute(), and a server that refuses prepared statements,
 * as Cloudflare Hyperdrive does, takes query(), each value held to how the session reads it
 * (asWritten).
 */
export async function mariadb(c: AnyClient, text: string, values: unknown[], columns: Columns | undefined, built?: SqlMode, marks?: Marks): Promise<unknown[]> {
  // By position where the module declares the columns: MySQL names a column with no alias after its text, which is `?` where the value is bound and the value itself where query() writes it in (`select :x::int + 1` is `CAST(5 AS SIGNED) + 1`), and two of them may share a name.
  const options = { sql: text, ...MARIADB_OPTIONS, rowsAsArray: Boolean(columns?.length), ...(columns?.some(([, kind]) => kind === "unknown" || kind.split("|").includes("geometry")) ? { typeCast: mariadbGeometry } : {}) };
  // The promise API resolves to the rows with their `meta`; the callback API hands the meta over as a third argument.
  let callbackMeta: MariadbColumn[] | undefined;
  const call = (conn: AnyClient, method: "execute" | "query") =>
    new Promise((resolve, reject) => {
      const returned = conn[method](options, values, (error: Error | null, rows: unknown, meta?: MariadbColumn[]) => (error ? reject(error) : ((callbackMeta = meta), resolve(rows))));
      if (typeof returned?.then === "function") returned.then(resolve, reject);
    });
  let result: unknown;
  // A connection's refusal is remembered on its info, which every wrapper a pool hands out over it shares.
  const key = (typeof c.info === "object" && c.info) || preparesKey(c);
  let prepared = typeof c.execute === "function" && !refusesPrepares.has(key);
  if (prepared) {
    const pool = typeof c.getConnection === "function" && typeof c.release !== "function";
    try {
      // On a MySQL server the driver sends a statement's EXECUTE once its PREPARE answers, and at once
      // where it has the statement prepared already, so one run could overtake another: each run on a
      // connection waits for the one before it to answer.
      result = await inTurn(pool ? undefined : c, true, async (handed) => {
        try {
          return await call(c, "execute");
        } finally {
          handed();
        }
      });
    } catch (e) {
      if (!isPrepareRefusal(e)) throw e;
      refusesPrepares.add(key);
      prepared = false;
    }
  }
  if (!prepared) {
    // Without values the driver writes nothing into the text, so no place can be misread. A query
    // defined by hand carries no reading of its own: where its text holds a backslash or a double
    // quote, which the sql_mode can read otherwise, it runs only on a session that reads it as the
    // driver's scan does.
    const placed = !values.length ? undefined : (built ?? (marks === undefined && /["\\]/.test(text) ? SCANS : undefined));
    scansAsBuilt("mariadb", placed);
    result = await asWritten(c, values, placed, (conn) => call(conn, "query"));
  }
  if (!Array.isArray(result)) return [];
  if (!result.length) return result;
  const meta = () => ((result as { meta?: MariadbColumn[] }).meta ?? callbackMeta ?? []) as MariadbColumn[];
  // execute() writes a TIME's fraction with six digits, or none where they are all zero, and mariadb
  // 3.0 a DATETIME's or a TIMESTAMP's with none where they are all zero: each takes its column's own
  // count, none for a column with more than six (an expression's, which MySQL reports as 31).
  if (prepared) {
    const timed = meta().flatMap((m, i) => (m.type === "TIME" || m.type === "DATETIME" || m.type === "TIMESTAMP" ? [[i, typeof m.name === "function" ? m.name() : m.name, (m.scale ?? 0) > 6 ? 0 : (m.scale ?? 0), m.type === "TIME"] as const] : []));
    for (const row of result as Record<string | number, unknown>[]) {
      for (const [i, name, scale, time] of timed) {
        const at = Array.isArray(row) ? i : name;
        if (at !== undefined && typeof row[at] === "string") row[at] = time ? timeOfScale(row[at] as string, scale) : withFraction(row[at] as string, scale);
      }
    }
  }
  // The module's columns as the result holds them, the last of each name: a row keeps that one's value.
  const cols = columns && Array.isArray(result[0]) ? resultColumns(columns, meta().map((m) => (typeof m.name === "function" ? m.name() : m.name) as string), result[0]) : (columns ?? []).map(([name, kind]) => [name, kind] as const);
  const rows = asObjects(result, cols);
  const own = lastOfEach(cols);
  // A DATETIME and a TIMESTAMP with `T`, and a SET's members joined as MySQL writes them: by the
  // module's kinds, else by the columns' metadata (mariadb 2 has no isSet()).
  const work: { name: string; date: boolean }[] = columns
    ? own.filter(([, kind]) => kind === "text" || kind?.split("|").includes("timestamp")).map(([name, kind]) => ({ name, date: kind !== "text" }))
    : meta()
        .map((m) => ({ name: typeof m.name === "function" ? m.name() : m.name, type: m.type, set: (typeof m.isSet === "function" && m.isSet()) || m.type === "SET" }))
        .filter((m): m is { name: string; type: string | undefined; set: boolean } => m.name !== undefined && (m.set || m.type === "DATETIME" || m.type === "TIMESTAMP"))
        .map((m) => ({ name: m.name, date: !m.set }));
  // A truth value a boolean; an INT a temporary table may widen to a BIGINT a BigInt, as the BIGINT is.
  fixRows(rows, kindFixes(own));
  if (!work.length) return rows;
  for (const row of rows) {
    for (const { name, date } of work) {
      const v = row[name];
      if (v === null || v === undefined) continue;
      if (Array.isArray(v)) row[name] = coerce("text", v, name);
      else if (date && typeof v === "string") row[name] = v.replace(" ", "T");
    }
  }
  return rows;
}

/** A result's column as the module declares it: its name, and its kind where the module has one of that column. */
type Declared = readonly [name: string, kind: Kind | undefined];

/**
 * A result's columns as the module declares them, in the result's order (`namesInResult`): the
 * module's own by position where the result is still theirs, else each of the result's by its
 * name, with the kind of the module's last column of that name, which is the one a row keeps.
 * `first` is the result's first row: where it is an array the result's names do not account for
 * (mariadb's callback API hands some rows over with no metadata), the module's stand by position.
 */
function resultColumns(columns: Columns | undefined, result: string[], first: unknown): Declared[] {
  if (!columns) return result.map((name) => [name, undefined] as const);
  const declared = columns.map(([name]) => name);
  const width = Array.isArray(first) ? first.length : result.length;
  if (columns.length === width && (result.length !== width || namesInResult(declared, result) === declared)) return columns.map(([name, kind]) => [name, kind] as const);
  const kinds = new Map(columns.map(([name, kind]) => [name, kind] as const));
  return result.map((name) => [name, kinds.get(name)] as const);
}

/** Rows a driver gave as arrays, as objects under the columns' names; a client that took no such option gives objects already, kept as they are. */
function asObjects(rows: unknown[], cols: readonly Declared[]): Record<string, unknown>[] {
  if (!Array.isArray(rows[0])) return rows as Record<string, unknown>[];
  return (rows as unknown[][]).map(rowOf(cols.map(([name]) => name)));
}

/** The fixes the columns' kinds call for, for a driver that tells each type by its value. */
function kindFixes(cols: readonly Declared[]): (readonly [string, Fix])[] {
  const fixes: (readonly [string, Fix])[] = [];
  for (const [name, kind] of cols) {
    const fix = byKind(kind);
    if (fix) fixes.push([name, fix]);
  }
  return fixes;
}

// ---------------------------------------------------------------------------------------------
// PlanetScale 1.x

type PlanetscaleField = { type: string; charset?: number };
/** PlanetScale hands each value over as a string of bytes, one character each. */
const bytesOf = (s: string) => {
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
};

/** PlanetScale's own default cast (cast.js), with a BIGINT a BigInt and a DATETIME's and a TIMESTAMP's `T`: it replaces any cast of the app's for this query. */
function planetscaleCast(field: PlanetscaleField, value: string | null): unknown {
  if (value === null || value === undefined) return value;
  switch (field.type) {
    case "INT64":
    case "UINT64":
      return BigInt(value);
    case "DATE":
    case "TIME":
    case "DECIMAL":
      return value;
    case "DATETIME":
    case "TIMESTAMP":
      return value.replace(" ", "T");
    case "JSON":
      return JSON.parse(utf8(bytesOf(value)));
    case "INT8":
    case "INT16":
    case "INT24":
    case "INT32":
    case "UINT8":
    case "UINT16":
    case "UINT24":
    case "UINT32":
    case "YEAR":
      return parseInt(value, 10);
    case "FLOAT32":
    case "FLOAT64":
      return parseFloat(value);
  }
  if (field.charset === 63) return bytesOf(value);
  return value ? utf8(bytesOf(value)) : "";
}

/** PlanetScale's execute(): 1.x binds parameters; 2.0 takes none, and its options come second. */
export async function planetscale(m: AnyClient, text: string, values: unknown[], columns: Columns | undefined, built?: SqlMode): Promise<unknown[]> {
  if (values.length) scansAsBuilt("@planetscale/database", built);
  // By position where the module declares the columns: the driver writes each value into the statement's text, so MySQL names a column with no alias after it, as on mariadb, and Vitess in front of a PlanetScale database writes such a name its own way.
  const options = { as: columns?.length ? "array" : "object", cast: planetscaleCast };
  let result;
  try {
    result = await m.execute(text, values.length ? values : null, options);
  } catch (error) {
    if (!/^Query parameters are not supported/.test((error as Error)?.message)) throw error;
    if (values.length) throw new TypeError(`@planetscale/database 2.0 binds no parameters, so reylo cannot pass this statement's values to it; use its 1.x line, or mysql2 on PlanetScale's MySQL endpoint.`, { cause: error });
    result = await m.execute(text, options);
  }
  // PlanetScale's result always carries its rows, an empty array for a statement that returns
  // none. Another driver's execute() (TiDB Cloud's answers with the rows themselves) would read
  // as no rows at all.
  if (!Array.isArray(result?.rows)) throw new TypeError(`reylo ran this statement through execute(), as on PlanetScale's @planetscale/database, and the client answered with ${Array.isArray(result) ? "an array" : result === null || typeof result !== "object" ? String(result) : "an object"} rather than a result with rows. It takes execute(text, values) as on mysql2 and PlanetScale, or a Drizzle database over one of them.`);
  if (!columns?.length || !result.rows.length) return result.rows;
  const cols = Array.isArray(result.rows[0]) ? resultColumns(columns, ((result.fields ?? []) as { name: string }[]).map((f) => f.name), result.rows[0]) : columns.map(([name, kind]) => [name, kind] as const);
  const rows = asObjects(result.rows, cols);
  fixRows(rows, kindFixes(lastOfEach(cols)));
  return rows;
}

// A binary parameter is typed Uint8Array, and every driver binds one as it is (pg-promise too,
// since it gets { text, values }) but two (lab/driver-matrix, S1c). mariadb writes any value
// that is not a Buffer as JSON, so a plain Uint8Array (TextEncoder's, getRandomValues') arrived
// as the text {"0":1,"1":2,"2":255}: it gets a Buffer over the same memory, where a Buffer
// exists (not in every Worker). Neon's neon() before 0.10 is the other (src/execute.ts, `asByteaHex`).
function asBuffer(v: unknown) {
  // Named structurally, so that a project without Node's types can type-check the module.
  const NodeBuffer = (globalThis as { Buffer?: { isBuffer(v: unknown): boolean; from(buffer: ArrayBufferLike, offset: number, length: number): Uint8Array } }).Buffer;
  return v instanceof Uint8Array && NodeBuffer !== undefined && !NodeBuffer.isBuffer(v) ? NodeBuffer.from(v.buffer, v.byteOffset, v.byteLength) : v;
}

/**
 * The statement and its values for mariadb and PlanetScale, which write each value into the text
 * themselves, a number as `String` writes it, so -0 as the integer `0`. A DOUBLE's or a FLOAT's -0
 * goes in at its mark as MySQL's float literal `-0e0`, the DOUBLE mysql2 binds, and the driver
 * writes the rest: the text `'-0'` would be a string wherever the place's type follows its value's,
 * as in `coalesce(:d, d)`. The literal goes in parentheses, since mariadb 2 reads `--` as a comment's
 * start with nothing after it, so `1-?` would hide every placeholder after it. Without marks, or
 * where the module carries the sql_mode its build read
 * the statement's strings under, a mark need not be a placeholder of the session's, and such a -0
 * is refused.
 */
function signedZeros(query: QueryModule<"mysql">, bound: Bound, driver: string): { text: string; values: unknown[] } {
  // A type its value decides between (`:i + :u`, a BIGINT or a DOUBLE) takes the DOUBLE -0 mysql2
  // binds, as a structured parameter's member does (src/rows.ts).
  const zero = (v: unknown, name: string) => {
    const type = query.inputs?.[name]?.type;
    return Object.is(v, -0) && type !== undefined && takesFloat(type, "mysql");
  };
  const written = query.sqlMode === undefined ? literalsAtMarks(bound, (v, i) => (zero(v, bound.names[i]!) ? "(-0e0)" : undefined)) : bound;
  // One left, where its place is unknown, would be written as 0.
  const left = written.values.findIndex((v, i) => zero(v, written.names[i]!));
  if (left >= 0) throw new ParameterError(`Parameter "${written.names[left]}" is -0, which ${driver} writes into the statement as 0, and Reylo writes it in as -0e0 only where it knows the placeholder's place, read under the default sql_mode: pass 0, or run the statement on mysql2.`);
  return written;
}

/**
 * mariadb and PlanetScale write an array as a list of values, one literal for each element, which
 * changes the statement: `json_set(doc, '$.name', ?)` given `["x", "$.role", "admin"]` sets a
 * second path. Every array a MySQL parameter takes (a VECTOR, a SET, a JSON value) is text by here.
 */
function noLists(driver: string, { values, names }: Bound) {
  values.forEach((v, i) => {
    if (Array.isArray(v)) throw new TypeError(`Parameter "${names[i]}" is an array, which ${driver} writes into the statement as a list of values, one for each element, where the statement takes one value. Pass one value, or the array's JSON text.`);
  });
}

/** The GeoJSON types mariadb writes an object of as a geometry, `ST_GeomFromText(…)` (text-encoder.js, GEO_TYPES). */
const GEO_TYPES = new Set(["Point", "LineString", "Polygon", "MultiPoint", "MultiLineString", "MultiPolygon", "GeometryCollection"]);

/**
 * A value as mariadb writes it with none of the app's options: an object, which it writes as its
 * JSON text, as that text, since the app's permitSetMultiParamEntries would write it as `key` =
 * value pairs, which a per-query option does not turn off on mariadb 3. An object the driver
 * writes otherwise stays the driver's to write: bytes, a Date (one of another realm made one of this
 * realm's), a stream (an object with pipe() and read()), a GeoJSON geometry and one with
 * toSqlString(); a boxed string goes as its string. A long.js Long goes as the BigInt it holds, which every version writes as
 * one integer: mariadb 2 writes a Long as its integer, and mariadb 3 as any other object.
 */
function asMariadbValue(v: unknown): unknown {
  if (v === null || typeof v !== "object" || v instanceof Uint8Array) return v;
  // mariadb 3.5's execute() tells a Date and a boxed string by this realm's constructors, and binds one of another realm as quoted JSON.
  const tag = Object.prototype.toString.call(v);
  if (tag === "[object Date]") return v instanceof Date ? v : new Date((v as Date).getTime());
  if (tag === "[object String]") return String(v);
  const own = v as { pipe?: unknown; read?: unknown; toSqlString?: unknown; type?: unknown; __isLong__?: unknown; low?: unknown; high?: unknown; unsigned?: unknown };
  // long.js marks a Long on its prototype, so a JSON document shaped like one stays a document.
  if (own.__isLong__ === true && !Object.hasOwn(v, "__isLong__") && Number.isInteger(own.low) && Number.isInteger(own.high)) {
    const bits = (BigInt((own.high as number) >>> 0) << 32n) | BigInt((own.low as number) >>> 0);
    return own.unsigned === true ? bits : BigInt.asIntN(64, bits);
  }
  if ((typeof own.pipe === "function" && typeof own.read === "function") || typeof own.toSqlString === "function" || GEO_TYPES.has(own.type as string)) return v;
  return JSON.stringify(v);
}

/** A MySQL statement's rows on each MySQL driver's client, or undefined on any other client. */
async function runMysql(c: Client, query: QueryModule<"mysql">, bound: Bound): Promise<unknown[] | undefined> {
  const { text, values, marks } = bound;
  if (typeof c.unsafe === "function") return bunRows(await c.unsafe(text, values), query.columns);
  // mariadb, checked before execute() because it has one too (returning rows, not a pair).
  // Its callback API (mariadb/callback) answers through the callback and returns a command
  // object; its promise API returns a promise and ignores the callback.
  if (typeof c.batch === "function" && typeof c.query === "function") {
    noLists("mariadb", bound);
    const signed = signedZeros(query, bound, "mariadb");
    return await mariadb(c, signed.text, signed.values.map((v) => asMariadbValue(asBuffer(v))), query.columns, query.sqlMode, marks);
  }
  // mysql2's callback API: the promise() wrapper runs on the same connection.
  const m = typeof c.promise === "function" ? c.promise() : c;
  if (typeof m.execute === "function") {
    // mysql2 has query() beside execute(); PlanetScale's Connection, Client and transaction do not.
    if (typeof m.query === "function") return await mysql2(m, text, values, query.columns, marks, query.sqlMode);
    noLists("@planetscale/database", bound);
    const signed = signedZeros(query, bound, "PlanetScale");
    return await planetscale(m, signed.text, signed.values, query.columns, query.sqlMode);
  }
  return undefined;
}

/** A column's raw value as its kind, from whatever form the driver read, as a Standard Query's `decode` takes it (src/standard-query.ts). */
function mysqlDecoder(name: string, kind: Kind, delim: string | undefined): (v: unknown) => unknown {
  // A MySQL DATETIME's text takes a T between the date and the time, as `.run()` writes it, a
  // column that may be one included (`mariadb`).
  if (kind.split("|").includes("timestamp")) return (v) => (typeof v === "string" ? v.replace(" ", "T") : coerce(kind, v, name, delim));
  // A MySQL GEOMETRY's raw value is its bytes, as `.run()` gives it. An object is a driver's own
  // reading of one (mysql2's `{ x, y }`, mariadb's GeoJSON), which the bytes cannot be had back from.
  if (kind.split("|").includes("geometry"))
    return (v) => {
      if (typeof v === "object" && v !== null && !(v instanceof Uint8Array)) throw new DecodeError(`Column "${name}" holds ${describe(v)}, which is no raw MySQL value: a GEOMETRY's raw value is its bytes, which a driver parses into an object unless its conversions are off (mysql2's typeCast returning \`field.buffer()\`).`);
      return coerce(kind, v, name, delim);
    };
  return (v) => coerce(kind, v, name, delim);
}

/** MySQL's half of the run time (src/execute.ts, `Runtime`), which `defineMysqlQuery` passes. */
export const mysql: Runtime<"mysql"> = { dialect: "mysql", run: runMysql, readError: readMysqlError, decoder: mysqlDecoder };
