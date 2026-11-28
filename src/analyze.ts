// One analysis path for every consumer of a `.sql` file — the type check, the bundler plugins and
// the runtime hook — and one cache they share: `reylo/.cache/` in the project
// file, one entry per statement. The cache is only for speed. An entry records the schema it was
// analyzed against; while the database answers, an entry for another schema is analyzed again,
// and while it does not, or no URL names it, the entries are used as they are, so a build that
// cannot reach the database runs on what an earlier type check or build wrote.
import { existsSync, mkdirSync, readdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { join, relative } from "node:path";
import { anyEngine, DIAGNOSTIC, engine, reason, MissingEngine, REYLO_VERSION, Unreachable, type Analysis, type Engine, type Lexed, type Schema } from "./engine.ts";
import { packageDir, resolveProject, sqlitePath, type Dialect, type ResolvedProject } from "./project.ts";
import { NotMysql, refusedLogin } from "./connection.ts";
import { keptError } from "./server-errors.ts";
import { NotPrepared, openProjectDrivers, placeRefusal, projectDriverVersions, type SqliteDriver } from "./sqlite-drivers.ts";

/** What `reylo/.cache/` holds for one statement. */
export type Entry = {
  version: 1;
  dialect: ResolvedProject["dialect"];
  schema: string;
  analysis: Analysis;
  /** When it was written, in milliseconds since 1970: a copy of `reylo/.cache/` keeps it where it may not keep the file's time. */
  at: number;
};

export type Outcome =
  | {
      ok: true;
      analysis: Analysis;
      source: "cache" | "database";
      entry: string;
      /**
       * Where the analysis is `reylo/.cache/`'s and nothing held it to the database's schema, why, and
       * when it was made: `cannot connect to the database at localhost:5432/app (DATABASE_URL in
       * .env): connection refused; building from reylo/.cache/, analyzed 2026-11-07`. The types are then
       * as old as the entry, so a build step prints the line and a hover shows it.
       */
      unverified?: string;
    }
  | { ok: false; message: string; code: number };

/**
 * A connection the engine holds (src/engine.ts): `analyze` runs a statement's whole analysis over
 * it, `query` runs a catalog query of the caller's, `schema` reads the schema's fingerprint by the
 * engine's own query, as every host reads it, and `forget` drops the answers the engine keeps once
 * the schema changes. On SQLite, `drivers` are the app's own, which each statement is prepared on
 * too (src/sqlite-drivers.ts).
 */
type Io = { analyze(sql: string, label?: string): Promise<{ analysis: Analysis; kept: boolean; after?: string; again?: boolean }>; query(sql: string, params: unknown[]): Promise<unknown[][]>; schema(): Promise<Schema>; forget(): Promise<void>; close(): Promise<void>; drivers?: SqliteDriver[] };

const ENTRY_VERSION = 1;
// How long a schema fingerprint is trusted, as long as a statement answer: a migration shows up
// in a running dev server or editor within seconds. Once the database is found unreachable, the
// session stays offline for a minute instead of paying the connect timeout per file.
const SCHEMA_TTL = 5_000;
/** How many times a statement is analyzed while the schema moves under its analysis, before the last analysis is given and written nowhere. */
const ANALYSES = 3;
/** How many databases' entries a statement keeps. */
const ENTRIES_KEPT = 4;
const OFFLINE_TTL = 60_000;
// A build step's session closes its connection a moment after its last file, so that a build
// process ends when its work does: the plugins whose bundler says when a build ends close it then
// (`releaseConnections`), and a loader (webpack, Rspack, Turbopack) has no such signal. The type
// check's session keeps its connection.
const BUILD_IDLE = 250;
/**
 * The URL parameters that choose the server or the database a session reads, beside the URL's
 * host and path, as each dialect's engine reads them (engine/native/src/): libpq's, among them
 * `user`, which names the role as the URL's user part does, and `options`, which may set the
 * search path a name is read through; mysql2's socket; and ClickHouse's database, the header that
 * names it, and the path in front of its interface. They name a session's entries with its server
 * (`#server`), as `server` in rust/reylo-build/src/analysis.rs names them.
 */
const CHOOSING: Record<Dialect, readonly string[]> = {
  postgres: ["dbname", "host", "hostaddr", "options", "port", "user"],
  mysql: ["socket", "socketPath"],
  clickhouse: ["ch_database", "clickhouse_setting_database", "http_header_x-clickhouse-database", "pathname"],
  sqlite: [],
};

const sessions = new Map<string, Session>();

/**
 * The project a directory belongs to, its file read by the engine's core, and the engine of its own
 * database, or why that cannot be run.
 */
async function openProject(dir: string, env: Record<string, string | undefined>): Promise<{ project: ResolvedProject; engine: Engine | MissingEngine }> {
  const project = resolveProject(await anyEngine(), dir, env);
  const own = await engine(project.dialect).catch((e: unknown) => (e instanceof MissingEngine ? e : new MissingEngine((e as Error).message)));
  return { project, engine: own };
}

export class Session {
  /** The project as resolved when the session opened, or again once a failed `direct_url` command succeeds (`#reread`). */
  project!: ResolvedProject;
  /** The project's directory. */
  readonly root: string;
  /** The engine of the project's database, or why there is none, which an analysis reports. */
  #engine!: Engine | MissingEngine;
  #engineFingerprint!: string;
  readonly #idle: number;
  /** Where the project was resolved from, and with what environment, for `#reread`. */
  readonly #dir: string;
  readonly #env: Record<string, string | undefined>;
  /** The variables of `ENVIRONMENT` as the session started, which its connection reads too: its entries keep one name however the process's environment changes after. */
  readonly #libpq = libpqEnvironment();
  #io: Promise<Io> | undefined;
  /** The URL the connection opens: the project's, until a command in `direct_url` gives a fresh one. */
  #url: string | undefined;
  #busy = 0;
  #timer: ReturnType<typeof setTimeout> | undefined;
  /** What `reylo/.cache/queries` held when this session first looked. */
  #listing: string[] | undefined;
  /** The schema's fingerprint read last, with its stamp where the database keeps one (`Schema`), and when. */
  #schema: { value: string | null; stamp?: number | null; at: number; error?: string; fatal?: boolean } | undefined;
  /** The fingerprint and stamp read last, or the unverified value that stood in for them, which a spell offline leaves in place: the engine's answers are of that schema. */
  #known: string | undefined;

  private constructor(engine: Engine | MissingEngine, project: ResolvedProject, idle: number, dir: string, env: Record<string, string | undefined>) {
    this.root = project.root;
    this.#idle = idle;
    this.#dir = dir;
    this.#env = env;
    this.#adopt(engine, project);
  }

  #adopt(engine: Engine | MissingEngine, project: ResolvedProject): void {
    this.#engine = engine;
    this.project = project;
    this.#url = project.url;
    this.#engineFingerprint = engine instanceof MissingEngine ? "unknown" : engine.cache;
  }

  /**
   * Reads the project again while a `direct_url` command's failure is its error, as when a
   * password manager was locked: the command runs again once its failure has stood a minute
   * (src/project.ts), so that a session kept open, as the type check's is, recovers without a
   * restart.
   */
  async #reread(): Promise<void> {
    if (!this.project.error || !this.project.refreshUrl) return;
    const { project, engine } = await openProject(this.#dir, this.#env);
    if (project.error) return;
    this.#adopt(engine, project);
    this.#schema = undefined;
  }

  /** The build steps' session for the project a file or directory belongs to: one per project (and connection) and process. */
  static async for(from: string, env: Record<string, string | undefined> = process.env): Promise<Session> {
    // From the package's directory, so that a message names a file as the project sees it.
    const dir = packageDir(from, env);
    const { project, engine } = await openProject(dir, env);
    // A setting the build step reads (`sqlcommenter`, `allow_nested_arrays`, and `dir`, which places the cache) is part of
    // the key, so a change to it in the project file reaches a running watch process.
    // So are libpq's variables, which choose the database the connection reaches and name its entries.
    const key = JSON.stringify([project.root, project.url ?? null, project.error ?? null, project.sqlcommenter, project.allowNestedArrays, project.cache, engine instanceof MissingEngine ? engine.message : null, libpqEnvironment()]);
    let session = sessions.get(key);
    if (!session) sessions.set(key, (session = new Session(engine, project, BUILD_IDLE, dir, env)));
    return session;
  }

  /** A session of its own, for a caller that must not share one (the type check, a test); it keeps its connection until closed. */
  static async open(from: string, env: Record<string, string | undefined> = process.env, idle = Infinity): Promise<Session> {
    const { project, engine } = await openProject(from, env);
    return new Session(engine, project, idle, from, env);
  }

  /** Every build-step session this process opened, for a caller that ends them (a test). */
  static opened(): Session[] {
    return [...sessions.values()];
  }

  async lex(sql: string): Promise<Lexed> {
    if (this.#engine instanceof MissingEngine) throw this.#engine;
    return this.#engine.lexAsync(sql, this.project.dialect);
  }

  /**
   * Where this session's entry for a statement lives, whether or not it exists yet: named by the
   * statement, then by the server. Two databases keep an entry each, and one whose schema matches
   * reads the other's (`#sameSchema`), so a fresh database built by the same migrations analyzes
   * nothing again. A session that cannot reach its database reads its own entry all the same, and
   * one with no URL the newest of any server's (`#offlineEntry`), each saying so (`#unverified`).
   */
  entryFile(sql: string): string {
    return join(this.project.cache, "queries", `${this.#statementKey(sql)}-${sha256(this.#server()).slice(0, 16)}.json`);
  }

  #statementKey(sql: string): string {
    return sha256(JSON.stringify([ENTRY_VERSION, this.#engineFingerprint, this.project.dialect, sql])).slice(0, 32);
  }

  /**
   * The statement's entry for a session that cannot reach its database, or has no URL: its own
   * server's, else the newest from any other where `others` allows. A URL that names a server and
   * fails reads that server's entry alone, since another's says nothing of the database the URL
   * asks for, and a mistyped host would otherwise build from whichever database was analyzed
   * last. A database that is a file has no server to mistake: its project built elsewhere, as in
   * a container, names the same file at another path.
   */
  #offlineEntry(sql: string, others = !this.project.url || this.project.dialect === "sqlite"): { file: string; entry: Entry } | undefined {
    const own = this.entryFile(sql);
    const entry = readEntry(own);
    if (entry) return { file: own, entry };
    if (!others) return undefined;
    const dir = join(this.project.cache, "queries");
    const prefix = `${this.#statementKey(sql)}-`;
    let names: string[];
    try {
      names = readdirSync(dir).filter((n) => n.startsWith(prefix) && n.endsWith(".json"));
    } catch {
      return undefined;
    }
    // Newest by the time the entry records, since a copy (`cp -r`, `fs.cpSync`, a CI artifact) may
    // reset the file's.
    let newest: { file: string; entry: Entry } | undefined;
    for (const file of names.map((n) => join(dir, n))) {
      const other = readEntry(file);
      if (other && (!newest || (other.at ?? -Infinity) > (newest.entry.at ?? -Infinity))) newest = { file, entry: other };
    }
    return newest;
  }

  /** The file names `reylo/.cache/queries` holds of a statement, by its key. */
  #entries(sql: string): string[] {
    const dir = join(this.project.cache, "queries");
    const prefix = `${this.#statementKey(sql)}-`;
    // One listing a session, however many statements it misses: what this session wrote it reads by
    // its own name, and an entry another process writes meanwhile is only a statement analyzed again.
    try {
      this.#listing ??= readdirSync(dir);
    } catch {
      this.#listing = [];
    }
    return this.#listing.filter((n) => n.startsWith(prefix) && n.endsWith(".json")).map((n) => join(dir, n));
  }

  /** Another database's entry for the statement, written against this schema. */
  #sameSchema(sql: string, schema: string): { file: string; entry: Entry } | undefined {
    for (const file of this.#entries(sql)) {
      const entry = readEntry(file);
      if (entry?.schema === schema) return { file, entry };
    }
    return undefined;
  }

  /** The statement's oldest entries gone, past a few: a per-build database has a new URL each time. */
  #prune(sql: string, written: string): void {
    const others = this.#entries(sql).filter((f) => f !== written);
    if (others.length < ENTRIES_KEPT) return;
    const at = (file: string) => readEntry(file)?.at ?? -Infinity;
    const oldest = others.map((file) => [at(file), file] as const).sort((a, b) => a[0] - b[0]);
    for (const [, file] of oldest.slice(0, others.length - ENTRIES_KEPT + 1)) rmSync(file, { force: true });
    this.#listing = undefined;
  }

  /**
   * The statement's analysis: from `reylo/.cache/` when the entry matches the schema, else from the
   * database. `store: false` neither reads nor writes `reylo/.cache/`, for a statement that is no file of
   * the project (`reylo play`'s, analyzed at each keystroke).
   */
  async analyze(sql: string, options: { force?: boolean; store?: boolean } = {}): Promise<Outcome> {
    clearTimeout(this.#timer);
    this.#busy++;
    try {
      await this.#reread();
      const outcome = await this.#analyze(sql, options);
      return outcome.ok && this.project.dialect === "sqlite" ? await this.#onDrivers(sql, outcome) : outcome;
    } finally {
      if (--this.#busy === 0 && this.#io && Number.isFinite(this.#idle)) this.#timer = setTimeout(() => this.#release(), this.#idle).unref();
    }
  }

  /** Rows of a catalog query on the session's connection, as arrays of values: what `reylo play` completes. */
  async query(sql: string, params: unknown[] = []): Promise<unknown[][]> {
    return (await this.#connection()).query(sql, params);
  }

  /**
   * A SQLite statement prepared on the app's own drivers too, whose SQLite may be older than the
   * engine's: the first that refuses it is an error at the place SQLite names. Not kept in
   * `reylo/.cache/`, since the drivers are the project's and not the database's; a session that cannot
   * open the database checks nothing. One whose connection was released while its schema is still
   * trusted opens it again, so a `reylo/.cache/` hit is checked however long the session sat idle.
   */
  async #onDrivers(sql: string, outcome: Extract<Outcome, { ok: true }>): Promise<Outcome> {
    const { analysis } = outcome;
    if (analysis.diagnostics.length || this.#schema?.value == null) return outcome;
    const io = await this.#connection().catch(() => undefined);
    for (const driver of io?.drivers ?? []) {
      let refusal: string | undefined;
      try {
        refusal = await driver.refuses(analysis.rewritten.sql, analysis.rewritten.order.length);
      } catch (e) {
        // Neither a refusal nor a pass: the next analysis asks the driver again.
        if (e instanceof NotPrepared) return { ok: false, message: `${driver.name} could not prepare this statement: ${e.message}`, code: DIAGNOSTIC.unreachable };
        throw e;
      }
      if (refusal === undefined) continue;
      const { start, length } = placeRefusal(sql, refusal);
      const message = driver.says(refusal);
      return { ...outcome, analysis: { ...analysis, diagnostics: [{ message, start, length, code: DIAGNOSTIC.refused }] } };
    }
    return outcome;
  }

  async #analyze(sql: string, options: { force?: boolean; store?: boolean }): Promise<Outcome> {
    if (this.project.error) return { ok: false, message: this.project.error, code: DIAGNOSTIC.project };
    // With no URL, `reylo/.cache/` answers as it does where the database cannot be reached (the dialect
    // is the one its entries record, src/project.ts), and a statement it lacks needs a URL.
    if (!this.project.url) {
      const hit = this.#engine instanceof MissingEngine || options.force ? undefined : this.#offlineEntry(sql);
      return hit ? { ok: true, analysis: hit.entry.analysis, source: "cache", entry: hit.file, unverified: this.#unverified(hit.entry) } : { ok: false, message: this.project.missingUrl, code: DIAGNOSTIC.noDatabase };
    }
    // The URL names the database, whose engine reylo has to be able to run.
    const engine = this.#engine;
    if (engine instanceof MissingEngine) return { ok: false, message: engine.message, code: DIAGNOSTIC.project };
    const file = this.entryFile(sql);
    let schema = await this.schema();
    if (schema.fatal) return { ok: false, message: schema.error!, code: DIAGNOSTIC.unreachable };
    if (schema.value === null) {
      const hit = options.force ? undefined : this.#offlineEntry(sql);
      if (hit) return { ok: true, analysis: hit.entry.analysis, source: "cache", entry: hit.file, unverified: this.#unverified(hit.entry, schema.error) };
      // Another database's analysis is no answer for the one the URL names.
      const elsewhere = !options.force && this.#offlineEntry(sql, true) !== undefined;
      const cache = this.#cacheName();
      const next = elsewhere ? `${cache} has this statement's analysis from another database only, which a build reads where no database URL is set: run the type check or a build where this database can be reached, or build with no URL` : `${cache} has no analysis of this statement, so run the type check or a build where the database can be reached first`;
      return { ok: false, message: `${this.cannotConnect(schema.error)}; ${next}.`, code: DIAGNOSTIC.unreachable };
    }
    const entry = options.force || options.store === false ? undefined : readEntry(file);
    // An entry for another fingerprint, written since this session read its own, is another
    // process's analysis of a schema that changed since (the type check's after a migration, while
    // a build step still trusts the fingerprint it read): the fingerprint is read again before
    // either is used, so the build step neither builds from the old schema nor writes it back.
    if (entry && entry.schema !== schema.value && entry.at >= this.#schema!.at) {
      await this.recheck();
      schema = await this.schema();
      if (schema.fatal) return { ok: false, message: schema.error!, code: DIAGNOSTIC.unreachable };
      if (schema.value === null) return { ok: true, analysis: entry.analysis, source: "cache", entry: file, unverified: this.#unverified(entry, schema.error) };
    }
    if (entry && entry.schema === schema.value) return { ok: true, analysis: entry.analysis, source: "cache", entry: file };
    // A database with this schema, at another URL: a fresh one built by the same migrations.
    const shared = options.force || options.store === false ? undefined : this.#sameSchema(sql, schema.value);
    if (shared) return { ok: true, analysis: shared.entry.analysis, source: "cache", entry: shared.file };
    let analysis: Analysis;
    let replicas = true;
    let after: string | undefined;
    let again: boolean | undefined;
    let label: string = schema.value;
    let stamp = this.#schema?.stamp;
    for (let tries = 1; ; tries++) {
      const io = this.#connection();
      try {
        // The label goes with the analysis only where it may be kept under it.
        ({ analysis, kept: replicas, after, again } = await (await io).analyze(sql, options.store === false || label.startsWith("unverified ") ? undefined : label));
      } catch (e) {
        // A dead connection is reopened by the next call; a server error is reported.
        if (e instanceof Unreachable) {
          this.#drop(io);
          this.#schema = { value: null, at: Date.now(), error: reason(e) };
          return { ok: false, message: this.cannotConnect(reason(e)), code: DIAGNOSTIC.unreachable };
        }
        if (e instanceof NotMysql) {
          this.#drop(io);
          this.#schema = { value: null, at: Date.now(), error: this.notMysql(e), fatal: true };
          return { ok: false, message: this.#schema.error!, code: DIAGNOSTIC.unreachable };
        }
        return { ok: false, message: (e as Error).message, code: DIAGNOSTIC.unreachable };
      }
      // A fingerprint the server refused to compute is this session's own, and labels nothing another reads.
      if (options.store === false || label.startsWith("unverified ")) break;
      // The fingerprint was read before the analysis, as long as SCHEMA_TTL ago, and the analysis is of
      // the schema as it was while it ran, with the answers the engine kept since the fingerprint last
      // moved. An entry labelled with a fingerprint that a migration has since moved would be served to
      // a database that still has the old schema, or has gone back to it, so the fingerprint is read
      // again now, and the entry written only where it has not moved. Where it has, the engine's
      // answers are dropped (`schema()`) and the statement analyzed again. Where the database keeps a
      // stamp (SQLite), a stamp that moved is a schema that changed in between, even one changed
      // back to the same fingerprint, so the statement is analyzed again then too.
      // On ClickHouse the engine read it after the analysis itself, from the servers that answered it.
      if (after !== undefined) await this.#took(await io, { fingerprint: after, stamp: null }, Date.now());
      else await this.recheck();
      const read = await this.schema();
      const now = read.value;
      const moved = this.#schema?.stamp;
      // One kept back for a replica that had not shown the schema yet, which the engine read
      // after the analysis, is analyzed again while the schema stands, unless the engine says no
      // try would keep it, as where it can't tell the servers that answered apart.
      if (now === label && moved === stamp && (replicas || again === false || tries === ANALYSES)) break;
      // A read that found the database gone leaves the next statement to try it again, rather than a minute offline.
      if (now === null && !read.fatal) this.#schema = undefined;
      if (now === null || now.startsWith("unverified ") || tries === ANALYSES) return { ok: true, analysis, source: "database", entry: file };
      label = now;
      stamp = moved;
    }
    // An analysis that carries an error the server raised for a reason of its own rather than the
    // statement's, which the engine's host fails the analysis with and which would stand for the
    // statement until the schema moved, is not kept, nor one a privilege decides, since a grant
    // moves no schema's fingerprint (src/server-errors.ts).
    // Nor is one whose answers came from replicas that did not show the schema it would be kept by
    // throughout, though nothing shows it wrong: the next analysis asks again.
    const kept = keptAnalysis(this.project.dialect, analysis);
    if (options.store !== false && kept && replicas) {
      writeEntry(this.project.cache, file, { version: ENTRY_VERSION, dialect: this.project.dialect, schema: label, analysis, at: Date.now() });
      this.#prune(sql, file);
    } else if (options.store !== false && !kept) {
      // An entry of an older schema is no answer where the database now refuses the statement, so
      // a build with the database out of reach does not build from it.
      try {
        rmSync(file, { force: true });
      } catch {}
      this.#listing = undefined;
    }
    return { ok: true, analysis, source: "database", entry: file };
  }

  /**
   * The fingerprint of the database's schema; null when the database cannot be reached, and
   * `fatal` when no connection can be made at all (no driver installed) or the server is one the type
   * check refuses (MariaDB, `NotMysql`). A server that refuses the
   * fingerprint query gets a value no entry matches, so every statement is analyzed live.
   */
  async schema(): Promise<{ value: string | null; error?: string; fatal?: boolean }> {
    const now = Date.now();
    if (this.#schema && now - this.#schema.at < (this.#schema.value === null ? OFFLINE_TTL : SCHEMA_TTL)) return this.#schema;
    const pending = this.#connection();
    let io: Io;
    try {
      io = await pending;
    } catch (e) {
      this.#drop(pending);
      this.#schema = e instanceof Unreachable ? { value: null, at: now, error: reason(e) } : { value: null, at: now, error: e instanceof NotMysql ? this.notMysql(e) : (e as Error).message, fatal: true };
      return this.#schema;
    }
    try {
      await this.#took(io, await io.schema(), now);
    } catch (e) {
      if (e instanceof Unreachable) {
        this.#drop(pending);
        this.#schema = { value: null, at: now, error: reason(e) };
      } else {
        // With no fingerprint to tell a change by, the engine's answers last as long as this value, and
        // are dropped once a fingerprint is read again.
        await io.forget().catch(() => {});
        const value = `unverified ${process.pid} ${now}`;
        this.#schema = { value, at: now };
        this.#known = value;
      }
    }
    return this.#schema!;
  }

  /** The fingerprint read at `at`, as the session's own. */
  async #took(io: Io, { fingerprint: value, stamp }: Schema, at: number): Promise<void> {
    // The engine's answers describe the old schema too, and outlast a spell offline: a schema
    // whose stamp moved may have been another in between, whatever its fingerprint is now.
    const known = JSON.stringify([value, stamp]);
    if (this.#known !== undefined && this.#known !== known) await io.forget().catch(() => {});
    this.#known = known;
    this.#schema = { value, stamp, at };
  }

  /**
   * What this session's analyses are a function of besides the statement: reylo's version, the
   * engine, the schema's fingerprint, and on SQLite the app's drivers each statement is prepared on
   * too. Where there is no fingerprint (no URL is set, the database cannot be reached, or it refuses
   * the query), the analyses come from `reylo/.cache/` or are not given, and the value is this process's
   * own, matching no other.
   */
  async identity(): Promise<string> {
    const value = this.project.url ? (await this.schema()).value : null;
    const drivers = this.project.dialect === "sqlite" ? projectDriverVersions(this.root) : [];
    return JSON.stringify([REYLO_VERSION, this.#engineFingerprint, value ?? `offline ${process.pid} ${Date.now()}`, drivers]);
  }

  /** Reads the schema's fingerprint again now, however recent the last read; whether it changed. */
  async recheck(): Promise<boolean> {
    const before = this.#schema?.value;
    if (this.#schema) this.#schema = { ...this.#schema, at: -Infinity };
    return (await this.schema()).value !== before;
  }

  /**
   * What an analysis read from `reylo/.cache/` with no schema to hold it to says of itself
   * (`Outcome.unverified`): why the database was not asked, and the day the entry was written.
   */
  #unverified(entry: Entry, why?: string): string {
    const found = this.project.url ? this.cannotConnect(why) : "no database URL is set";
    const day = Number.isFinite(entry.at) ? `, analyzed ${new Date(entry.at).toISOString().slice(0, 10)}` : "";
    return `${found[0]!.toLowerCase()}${found.slice(1)}; building from ${this.#cacheName()}/${day}`;
  }

  /** The cache as a message names it: from the project's root, as `reylo/.cache`. */
  #cacheName(): string {
    return relative(this.project.root, this.project.cache).replaceAll("\\", "/");
  }

  /** Where the database was looked for, and where that came from; never the credentials. */
  cannotConnect(why?: string): string {
    const { dialect, url, root, urlSource } = this.project;
    if (dialect === "sqlite") return `Cannot open the database ${sqlitePath(url ?? "", root)} (${urlSource})${why ? `: ${why}` : ""}`;
    return `Cannot connect to the database at ${this.where()} (${urlSource})${why ? `: ${why}` : ""}`;
  }

  /** Why a server the type check cannot read is refused, and what to point it at instead. */
  notMysql(e: NotMysql): string {
    return `Cannot type-check against the database at ${this.where()} (${this.project.urlSource}): it is ${e.server}, and Reylo type-checks MySQL statements against MySQL. Point the type check's URL at a MySQL server, 8.4 or later, with the same schema.`;
  }

  /**
   * The server, without credentials, that names this session's entries: none with no URL, which
   * writes none. The URL's parameters that choose the server or the database follow it, so that
   * two databases they tell apart keep an entry each.
   */
  #server(): string {
    const { dialect, url, root } = this.project;
    if (!url) return "";
    if (dialect === "sqlite") return sqlitePath(url, root);
    return serverName(url, dialect, this.#libpq);
  }

  /** The database as a person names it: host, port and database, or the SQLite file. */
  where(): string {
    const { dialect, url, root } = this.project;
    if (dialect === "sqlite") return sqlitePath(url ?? "", root);
    try {
      const u = new URL(url ?? "");
      return `${u.hostname || u.searchParams.get("host") || "localhost"}${u.port ? `:${u.port}` : ""}${u.pathname}`;
    } catch {
      return "the database";
    }
  }

  async close(): Promise<void> {
    for (const [key, session] of sessions) if (session === this) sessions.delete(key);
    await this.release();
  }

  /** Closes the connection now, for a caller that knows no file is coming; the next analysis opens another. */
  async release(): Promise<void> {
    clearTimeout(this.#timer);
    await this.#release();
  }

  /** Closes the connection; the next analysis opens another. */
  async #release(): Promise<void> {
    const io = this.#io;
    this.#io = undefined;
    if (io) await io.then((i) => i.close()).catch(() => {});
  }

  #connection(): Promise<Io> {
    this.#io ??= (async (): Promise<Io> => {
      let first: Io;
      try {
        first = await this.#open(this.#url!);
      } catch (e) {
        const fresh = this.#freshUrl(e);
        if (fresh === undefined) throw e;
        first = await this.#open(fresh);
      }
      if (!this.project.refreshUrl) return first;
      // A refused login can surface at any call, where a token expired while the session was open:
      // the call is asked once more on a connection with the command's fresh output. The calls refused
      // together, as every call waiting when a token expires is, share one reopen, so that the
      // command runs once and one connection replaces the old; a call that fails on a connection
      // already replaced is asked again on the one that replaced it.
      let current: Promise<Io> = Promise.resolve(first);
      const again = async <T>(call: (on: Io) => Promise<T>): Promise<T> => {
        const used = current;
        const on = await used;
        try {
          return await call(on);
        } catch (e) {
          if (current === used) {
            if (!refusedLogin(e)) throw e;
            current = (async () => {
              await on.close().catch(() => {});
              const fresh = this.#freshUrl(e);
              if (fresh === undefined) throw e;
              return this.#open(fresh);
            })();
            current.catch(() => {});
          }
          return call(await current);
        }
      };
      return {
        analyze: (sql, label) => again((on) => on.analyze(sql, label)),
        query: (text, params) => again((on) => on.query(text, params)),
        schema: () => again((on) => on.schema()),
        forget: () => again((on) => on.forget()),
        close: () => current.then((on) => on.close(), () => {}),
        ...(first.drivers && { drivers: first.drivers }),
      };
    })();
    const io = this.#io;
    io.catch(() => {
      if (this.#io === io) this.#io = undefined;
    });
    return io;
  }

  /**
   * Forgets a connection a call found dead, and closes it, since one opened in its place with a
   * command's fresh output may be live. Only the connection that call ran on: a call that fails
   * late never ends one opened since.
   */
  #drop(io: Promise<Io>): void {
    if (this.#io !== io) return;
    this.#io = undefined;
    io.then((i) => i.close()).catch(() => {});
  }

  /**
   * A URL to try instead, when the server refused the login and `direct_url`'s command now prints
   * something else, as when a short-lived password has expired; else undefined, so that nothing
   * is tried twice on the same password.
   */
  #freshUrl(e: unknown): string | undefined {
    if (!refusedLogin(e)) return undefined;
    const fresh = this.project.refreshUrl?.();
    if (fresh === undefined || fresh === this.#url) return undefined;
    this.#url = fresh;
    return fresh;
  }

  async #open(url: string): Promise<Io> {
    const { dialect, root } = this.project;
    // The URL names a database whose engine reylo cannot run: its error, as the import's.
    if (this.#engine instanceof MissingEngine) throw this.#engine;
    // The engine opens the database itself: on SQLite the file the project names, and the statement
    // is prepared on the app's own SQLite drivers too, which are the app's, not the engine's.
    const target = dialect === "sqlite" ? sqlitePath(url, root) : url;
    // libpq's variables as the session started, which name its entries, set and unset alike.
    const c = await this.#engine.open(target, Object.fromEntries(ENVIRONMENT.map((name) => [name, this.#libpq[name]])));
    let drivers: SqliteDriver[] | undefined;
    try {
      drivers = dialect === "sqlite" ? await openProjectDrivers(root, target) : undefined;
    } catch (e) {
      await c.close();
      throw e;
    }
    return {
      analyze: (sql, label) => c.analyzeFor(sql, label),
      query: (text, params) => c.query(text, params),
      schema: () => c.schema(),
      forget: () => c.forget(),
      close: async () => {
        await Promise.all((drivers ?? []).map((d) => d.close().catch(() => {})));
        await c.close();
      },
      ...(drivers && { drivers }),
    };
  }
}

/** Closes every build-step session's connection, for a build step whose build has ended: the process then ends without waiting out the idle time. */
export async function releaseConnections(): Promise<void> {
  await Promise.all([...sessions.values()].map((session) => session.release()));
}

const sha256 = (text: string) => createHash("sha256").update(text).digest("hex");

/**
 * The server `url` names, without credentials: the scheme, the user, the hosts and ports, and the
 * path, read off the text as `server` in rust/reylo-build/src/analysis.rs reads them (both are held
 * to entry-name-cases.json), since `URL` refuses a URL libpq reads, as `postgres://app@/shop` and a
 * list of hosts. Then the parameters that choose the server or the database (`CHOOSING`), as
 * `?name=value&…`, or nothing where it has none, which keeps the name its entries were written
 * under. A name is matched decoded, as the engine and the bridge to a Unix socket read it
 * (src/host-url.ts), and a header's in any case, as HTTP compares header names. A name and its
 * value are kept as written, since their readers decode a value each their own way (libpq reads a
 * `+` as a plus, ClickHouse and mysql2 as a space), so that a database written two ways is at most
 * analyzed again under a name of its own. They keep the URL's order, which decides which of
 * `socket` and `socketPath` the engine opens. On Postgres, the variables the engine and the bridge
 * read for a part the URL leaves out (`ENVIRONMENT`) follow them, encoded.
 */
function serverName(url: string, dialect: Dialect, env: Record<string, string | undefined>): string {
  const q = url.indexOf("?");
  const [base, query] = q < 0 ? [url, ""] : [url.slice(0, q), url.slice(q + 1)];
  const s = base.indexOf("://");
  const [scheme, rest] = s < 0 ? ["", base] : [base.slice(0, s), base.slice(s + 3)];
  const p = rest.indexOf("/");
  const [authority, path] = p < 0 ? [rest, ""] : [rest.slice(0, p), rest.slice(p)];
  const a = authority.lastIndexOf("@");
  const [user, host] = a < 0 ? ["", authority] : [authority.slice(0, a).split(":")[0]!, authority.slice(a + 1)];
  const chosen = query ? query.split("&").filter((pair) => CHOOSING[dialect].includes(listed(pair))) : [];
  if (dialect === "postgres") {
    const named = new Set(chosen.map(listed));
    // The authority's hosts, each with its port where it writes one: an IPv6 address in brackets.
    const hosts = host.split(",").filter(Boolean).map((chunk) => /^\[[^\]]*\](?::(.*))?$/.exec(chunk)?.[1] ?? (chunk.startsWith("[") ? undefined : chunk.split(":")[1]));
    const leftOut: Record<(typeof ENVIRONMENT)[number], boolean> = {
      PGHOST: hosts.length === 0 && !named.has("host") && !named.has("hostaddr"),
      // Several hosts with no port take 5432 each, as libpq reads them.
      PGPORT: hosts.length <= 1 && !hosts.some((port) => port) && !named.has("port"),
      PGUSER: !user && !named.has("user"),
      PGDATABASE: (path === "" || path === "/") && !named.has("dbname"),
      PGOPTIONS: !named.has("options"),
    };
    for (const name of ENVIRONMENT) if (leftOut[name] && env[name]) chosen.push(`${name}=${encodeURIComponent(env[name])}`);
  }
  return `${scheme}://${user}@${host}${path}${chosen.length ? `?${chosen.join("&")}` : ""}`;
}

/**
 * The variables libpq reads for a part of the URL it leaves out, as the engine reads them
 * (engine/native/src/postgres.rs), but for the system's user name, which a URL with no user takes
 * on every machine it is copied to, so that a cache built in CI is still read on a laptop.
 */
const ENVIRONMENT = ["PGHOST", "PGPORT", "PGUSER", "PGDATABASE", "PGOPTIONS"] as const;

/**
 * The variables of `ENVIRONMENT` this process has, as the engine's connection reads them when it
 * opens (src/engine.ts): on Windows in any case of a name, as Windows reads one, since a worker
 * thread's copy of the environment, as the preload's, matches names exactly, and of two spellings
 * the first in order, as Node passes a child.
 */
export function libpqEnvironment(env: NodeJS.ProcessEnv = process.env, platform: NodeJS.Platform = process.platform): Record<string, string> {
  const value = (name: string) => (platform === "win32" ? env[Object.keys(env).filter((key) => key.toUpperCase() === name).sort()[0] ?? name] : env[name]);
  return Object.fromEntries(ENVIRONMENT.flatMap((name) => (value(name) ? [[name, value(name)!]] : [])));
}

/**
 * A URL parameter's name as `CHOOSING` lists it: decoded, or as written where it does not decode,
 * and a header's in lower case.
 */
function listed(pair: string): string {
  let name = pair.split("=")[0]!;
  try {
    name = decodeURIComponent(name);
  } catch {}
  return name.startsWith("http_header_") ? name.toLowerCase() : name;
}

/** Whether an analysis may stand in `reylo/.cache/`: none of its diagnostics carries an error no analysis is kept with (src/server-errors.ts). */
function keptAnalysis(dialect: Dialect, analysis: Analysis): boolean {
  return analysis.diagnostics.every((d) => d.server_code === undefined || keptError(dialect, d.server_code, d.message));
}

function readEntry(file: string): Entry | undefined {
  if (!existsSync(file)) return undefined;
  try {
    const entry = JSON.parse(readFileSync(file, "utf8")) as Entry;
    // An entry that carries an error no analysis is kept with, as one an older build wrote, is none.
    return entry.version === ENTRY_VERSION && keptAnalysis(entry.dialect, entry.analysis) ? entry : undefined;
  } catch {
    return undefined;
  }
}

// Several processes write the cache at once (an editor's type check, a dev server, a build), so an
// entry is written to a file of its own and renamed into place: a reader sees a whole entry or
// none. Its own `.gitignore` keeps the cache out of git, as pytest's and ruff's caches do, in the
// `reylo/` directory whose other files are committed. The cache is only for speed, so a directory
// that cannot be written (a read-only checkout) is not an error.
function writeEntry(cache: string, file: string, entry: Entry): void {
  const dir = cache;
  const tmp = `${file}.${process.pid}.${Math.random().toString(36).slice(2)}.tmp`;
  try {
    mkdirSync(join(dir, "queries"), { recursive: true });
    const ignore = join(dir, ".gitignore");
    if (!existsSync(ignore)) writeFileSync(ignore, "# reylo's cache: written by the type check, the build steps and the migrations; never committed.\n*\n");
    writeFileSync(tmp, JSON.stringify(entry) + "\n");
    renameSync(tmp, file);
  } catch {
    rmSync(tmp, { force: true });
  }
}
