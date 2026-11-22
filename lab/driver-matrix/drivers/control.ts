// Failing controls: clients that are deliberately wrong, so every row here must FAIL. They show
// the scenarios can fail at all (a green matrix with no failing control is no evidence).
//   --case pg      PGlite behind a proxy whose query() binds each integer parameter plus one and a
//                  plain Uint8Array as its JSON text, as mariadb does without a Buffer; a client of
//                  Bun.sql's shape over PGlite whose unsafe() binds an array as its elements joined
//                  by commas, as Bun.sql does, running statements without their inputs, so reylo
//                  converts no array; one whose unsafe() binds every array as its array literal, a
//                  JSON parameter's too, as reylo did on Bun.sql at 0d82fea; and a transaction
//                  whose "rollback" commits and whose failure is swallowed. Reylo refuses an object
//                  of no driver's shape before it runs anything, so each broken client keeps the
//                  shape of the driver it fakes: a proxy over PGlite, or Bun.sql's commitDistributed()
//   --case sqlite  a node:sqlite database declared asynchronous, so runSync must refuse it and does not;
//                  and a transaction(fn) whose statements answer through a promise, so runSync must throw
import { arrayLiteral } from "../../../src/execute.ts";
import { args, callbackTx, requireFrom, runJob, seedSql, type TxOps } from "../lib/harness.ts";

const a = args();

if (a.case === "sqlite") {
  const { DatabaseSync } = await import("node:sqlite");
  const meta = { pkg: "control", version: "sqlite", target: "node:sqlite, declared asynchronous" };
  await runJob(meta, async () => {
    const db = new DatabaseSync(":memory:");
    db.exec(seedSql("sqlite"));
    // better-sqlite3's transaction(fn), nesting as savepoints, over statements whose all() is async.
    let depth = 0;
    const promised = {
      prepare: (sql: string) => {
        const statement = db.prepare(sql);
        return { all: async (...params: any[]) => statement.all(...params) };
      },
      transaction: (fn: () => unknown) => () => {
        const name = `sp${depth}`;
        db.exec(depth ? `SAVEPOINT ${name}` : "BEGIN");
        depth++;
        try {
          const result = fn();
          depth--;
          db.exec(depth ? `RELEASE ${name}` : "COMMIT");
          return result;
        } catch (e) {
          depth--;
          db.exec(depth ? `ROLLBACK TO ${name}; RELEASE ${name}` : "ROLLBACK");
          throw e;
        }
      },
    };
    return {
      ...meta,
      dialect: "sqlite",
      count: async (tag) => (db.prepare("select count(*) as n from scratch where tag = ?").get(tag) as any).n,
      shapes: [
        { name: "sync client declared async", kind: "async", scope: (fn) => fn(db) },
        { name: "db.transaction(fn) whose statements answer through a promise", kind: "sync", syncTx: promised },
      ],
    };
  });
} else {
  const { PGlite } = await import(requireFrom.resolve("pglite-05"));
  const meta = { pkg: "control", version: "pg", target: "PGlite behind a corrupting wrapper" };
  await runJob(meta, async () => {
    const db = new PGlite();
    await db.exec(seedSql("postgres"));
    const corruptValue = (v: unknown) => (typeof v === "number" ? v + 1 : v instanceof Uint8Array && !Buffer.isBuffer(v) ? JSON.stringify(v) : v);
    // A proxy over the PGlite instance, so Reylo runs it as PGlite; only query() corrupts its values. A
    // property defined on the proxy (the harness's spy on query()) stays on the proxy, so the instance the
    // other shapes and the raw counts use is never touched.
    const query = db.query.bind(db);
    const corrupting = (text: string, values?: unknown[], options?: unknown) => query(text, values?.map(corruptValue), options as any);
    const own = new Map<PropertyKey, PropertyDescriptor>();
    const corrupt = new Proxy(db, {
      get(target, key) {
        if (own.has(key)) return own.get(key)!.value;
        if (key === "query") return corrupting;
        const value = Reflect.get(target, key, target);
        return typeof value === "function" ? value.bind(target) : value;
      },
      defineProperty: (_target, key, descriptor) => (own.set(key, descriptor), true),
      deleteProperty: (_target, key) => (own.delete(key), true),
      getOwnPropertyDescriptor: (target, key) => own.get(key) ?? Reflect.getOwnPropertyDescriptor(target, key),
    });
    // Bun.sql's shape: unsafe(text, values) beside commitDistributed(), which Reylo tells Bun.sql's clients by. It
    // has no options, so Reylo reads the rows unsafe() resolves to rather than .raw()'s bytes.
    const unsafe = (bind: (v: unknown) => unknown) => ({
      unsafe: async (text: string, values?: unknown[]) => (await db.query(text, values?.map(bind))).rows,
      commitDistributed: async () => {
        throw new Error("the control's Bun.sql shape has no distributed transactions");
      },
    });
    const joined = unsafe((v) => (Array.isArray(v) ? v.join(",") : v));
    // Reylo hands unsafe() a JSON parameter as an object whose toJSON() returns the value.
    const literal = unsafe((v) => {
      const x = v !== null && typeof v === "object" && !(v instanceof Date) && typeof (v as { toJSON?: unknown }).toJSON === "function" ? (v as { toJSON(): unknown }).toJSON() : v;
      return Array.isArray(x) ? arrayLiteral(x) : v;
    });
    const good = callbackTx((fn) => db.transaction(fn));
    const broken: TxOps = {
      commit: good.commit,
      // "Rolls back" by committing.
      rollback: (body) => db.transaction((tx: any) => body(tx)),
      // Runs both writes and never rolls the savepoint back.
      savepoint: (outer, inner) =>
        db.transaction(async (tx: any) => {
          await outer(tx);
          await inner(tx);
        }),
      // Swallows the error, so the transaction commits the first write and nothing rejects.
      failing: (body) => db.transaction((tx: any) => body(tx).catch(() => {})),
    };
    return {
      ...meta,
      dialect: "postgres",
      count: async (tag) => (await db.query("select count(*)::int as n from driver_matrix.scratch where tag = $1", [tag])).rows[0].n,
      shapes: [
        { name: "parameters corrupted: an integer plus one, a Uint8Array as JSON", kind: "async", scope: (fn) => fn(corrupt) },
        { name: "unsafe() binding an array as a,b, statements without inputs", kind: "async", untyped: true, scope: (fn) => fn(joined) },
        { name: "unsafe() binding every array as its array literal, a JSON parameter's too", kind: "async", scope: (fn) => fn(literal) },
        { name: "rollback that commits", kind: "async", tx: broken },
      ],
      close: () => db.close(),
    };
  });
}
