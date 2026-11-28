import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { Session } from "../src/analyze.ts";
import { sqlModule } from "../src/loader.ts";
import { renderType } from "../src/render.ts";
import { columnKind, installPostgresTypes, textDecoder } from "../src/values.ts";
import { postgresTypes } from "../src/postgres-types.ts";
import { pg as pgType } from "./descriptors.ts";
import { DRIVERS, INDEX, MYSQL, driver, mysqlDatabase, mysqlLive, pgLive, postgresDatabase } from "./helpers.ts";

// Values at the edges of what each type holds, as `.run()` gives them.

// Each Postgres type's values at the edges of what it holds, as the server writes them.
const EDGES: Record<string, string[]> = {
  int2: ["-32768", "32767"], int4: ["-2147483648", "2147483647"], int8: ["-9223372036854775808", "9223372036854775807"], oid: ["0", "4294967295"],
  float4: ["-0", "3.4028235e+38", "1e-45"], float8: ["-0", "1.7976931348623157e+308", "5e-324"],
  numeric: ["NaN", "Infinity", "-Infinity", "-0.000000001", "1000000000000000000000000000000"], money: ["-$92,233,720,368,547,758.08", "$92,233,720,368,547,758.07"],
  date: ["4714-11-24 BC", "0001-01-01 BC", "9999-12-31", "10000-01-01", "5874897-12-31", "infinity", "-infinity"],
  timestamp: ["4714-11-24 00:00:00 BC", "294276-12-31 23:59:59.999999", "infinity", "-infinity"],
  timestamptz: ["4714-11-24 00:00:00+00 BC", "294276-12-31 23:59:59.999999+00", "1849-12-31 19:03:58-04:56:02", "2026-01-02 06:00:00+30", "12345-01-05 04:00:00+100", "12344-12-25 00:00:00-168", "infinity", "-infinity"],
  time: ["00:00:00", "23:59:59.999999", "24:00:00"], timetz: ["00:00:00+15:59", "24:00:00-15:59"],
  interval: ["-178000000 years", "178000000 years", "-00:00:00.000001", "infinity", "-infinity"],
  bytea: ["\\x"], text: [""], uuid: ["00000000-0000-0000-0000-000000000000"],
};

// Postgres's rarer types (a money, a range among them), as a module of one of them installs them.
installPostgresTypes(postgresTypes);

test("every Postgres type's values at its edges decode as the type the module declares", () => {
  for (const [name, texts] of Object.entries(EDGES)) {
    const t = pgType(name);
    const declared = renderType(t, false, "postgres");
    for (const text of texts) {
      const value = textDecoder(columnKind(t, "postgres"))(text);
      assert.equal(value instanceof Uint8Array ? "Uint8Array" : typeof value, declared, `${name} ${text}`);
    }
  }
});

test("Postgres: a date or a timestamptz outside 0000–9999 comes back in ISO 8601's extended year as a column and in Postgres's own year inside a document, and goes back in as a parameter", { skip: pgLive, timeout: 60_000 }, async () => {
  const own = await postgresDatabase(
    "reylo_years",
    `create table e (id int primary key, d date, at timestamptz, ds date[], r daterange, tr tstzrange);
    insert into e values
      (1, '0002-06-01 BC', '12345-01-01 00:00:00+00', array['12345-01-01'::date, '0002-06-01 BC'], '[0002-06-01 BC, 12345-01-01)', '[4713-01-01 00:00:00+00 BC, 12345-01-01 00:00:00.5+00)'),
      (2, '2026-06-01', '2026-01-01 00:00:00+00', array['2026-01-01'::date], 'empty', null)`,
  );
  const root = mkdtempSync(join(tmpdir(), "reylo-years-"));
  writeFileSync(join(root, "package.json"), "{}");
  writeFileSync(join(root, ".env"), `DATABASE_URL=${own.url}\n`);
  mkdirSync(join(root, "node_modules"));
  symlinkSync(join(DRIVERS, "postgres"), join(root, "node_modules", "postgres"));
  const built = async (name: string, sql: string) => {
    const file = join(root, `${name}.sql`);
    writeFileSync(file, sql);
    const { code } = await sqlModule(file, sql);
    const out = join(root, `${name}.mjs`);
    writeFileSync(out, code.replace(`from "reylo"`, `from ${JSON.stringify(INDEX)}`));
    return (await import(pathToFileURL(out).href)).default;
  };
  try {
    const columns = await built("columns", "select d, at, ds, r, tr from e order by id");
    const document = await built("document", "select jsonb_build_object('d', d, 'at', lower(tr)) as o, json_build_array(d, lower(tr)) as a from e order by id");
    const param = await built("param", "select id from e where d = :d::date and at = :at::timestamptz");
    const rows = await columns.run(own.sql);
    assert.deepEqual(rows[0], { d: "-000001-06-01", at: "+012345-01-01T00:00:00.000000Z", ds: ["+012345-01-01", "-000001-06-01"], r: { lower: "-000001-06-01", upper: "+012345-01-01", bounds: "[)" }, tr: { lower: "-004712-01-01T00:00:00.000000Z", upper: "+012345-01-01T00:00:00.500000Z", bounds: "[)" } });
    // Inside a document Postgres writes its own year, and an instant before a zone's first standard time with an offset to the second.
    await own.sql.unsafe("set time zone 'America/New_York'");
    const documents = await document.run(own.sql);
    assert.deepEqual(documents[0], { o: { d: "0002-06-01 BC", at: "4714-12-31T19:03:58-04:56:02 BC" }, a: ["0002-06-01 BC", "4714-12-31T19:03:58-04:56:02 BC"] });
    // A POSIX session zone takes the offset there past RFC 3339's 23:59, as far as 169 hours: the 168 Postgres reads, and
    // an hour of summer time, which the second zone keeps in January.
    await own.sql.unsafe("set time zone 'XXX-30'");
    const posix = await document.run(own.sql);
    assert.deepEqual(posix[0].a, ["0002-06-01 BC", "4713-01-02T06:00:00+30:00 BC"]);
    await own.sql.unsafe("set time zone 'XXX-167:59:60YYY,M10.1.0,M3.3.0'");
    const furthest = await document.run(own.sql);
    assert.deepEqual(furthest[0].o, { d: "0002-06-01 BC", at: "4713-01-08T01:00:00+169:00 BC" });
    // A year from 1 to 9999 takes those offsets too, which the format refuses.
    const fourDigits = await built("four-digits", "select jsonb_build_object('at', at, 'lmt', timestamptz '1850-01-01 00:00:00+00') as o, json_build_array(at, timestamptz '1850-01-01 00:00:00+00') as a from e where id = 2");
    await own.sql.unsafe("set time zone 'America/New_York'");
    const localMean = await fourDigits.run(own.sql);
    // One row by its key, which `.run()` gives alone.
    assert.deepEqual(localMean.a, ["2025-12-31T19:00:00-05:00", "1849-12-31T19:03:58-04:56:02"]);
    await own.sql.unsafe("set time zone 'XXX-30'");
    const posixFour = await fourDigits.run(own.sql);
    assert.deepEqual(posixFour.o, { at: "2026-01-02T06:00:00+30:00", lmt: "1850-01-02T06:00:00+30:00" });
    // A range and a multirange inside a document are their text.
    const ranges = await built("ranges", "select jsonb_build_object('r', r) as o, json_build_array(r, int4multirange(int4range(1, 3), int4range(5, 7))) as a from e order by id");
    const rangeRows = await ranges.run(own.sql);
    assert.deepEqual(rangeRows.map((row: { a: unknown }) => row.a), [['["0002-06-01 BC",12345-01-01)', "{[1,3),[5,7)}"], ["empty", "{[1,3),[5,7)}"]]);
    const back = { d: rows[0].d, at: rows[0].at };
    assert.deepEqual(await param.run(own.sql, back), [{ id: 1 }]);
  } finally {
    for (const session of Session.opened()) await session.close();
    await own.drop();
  }
});

test("MySQL: a CHAR comes back without its padding, or with it where the session keeps it, and text of the most bytes a character stores, a latin1 TEXT at its longest and a decimal at its most digits come back whole, on mysql2 and mariadb", { skip: mysqlLive, timeout: 60_000 }, async () => {
  // Each column at its longest in characters of the most bytes it stores, and each decimal at its most digits.
  const own = await mysqlDatabase(
    "reylo_value_edges",
    `create table sized (id int primary key, name varchar(20) not null, code char(5), legacy varchar(10) character set latin1, body text, old_body text character set latin1, price decimal(10,2), part decimal(5,5), at datetime(3), span time(4));
    insert into sized values
      (1, repeat('😀', 20), 'abcde', repeat('é', 10), repeat('😀', 16383), repeat('é', 65535), 99999999.99, 0.99999, '2026-01-01 12:00:00.123', '-838:59:59.0000'),
      (2, '', 'ab   ', 'a', repeat('z', 65535), '', -99999999.99, -0.00001, '2026-01-01 12:00:00', '12:00:00.1235'),
      (3, 'x', null, null, null, null, null, null, null, null)`,
  );
  const root = mkdtempSync(join(tmpdir(), "reylo-value-edges-"));
  writeFileSync(join(root, "package.json"), "{}");
  writeFileSync(join(root, ".env"), `DATABASE_URL=${own.url}\n`);
  mkdirSync(join(root, "node_modules"));
  for (const name of ["mysql2", "mariadb"]) symlinkSync(join(DRIVERS, name), join(root, "node_modules", name));
  const built = async (name: string, sql: string) => {
    const file = join(root, `${name}.sql`);
    writeFileSync(file, sql);
    const { code } = await sqlModule(file, sql);
    const out = join(root, `${name}.mjs`);
    writeFileSync(out, code.replace(`from "reylo"`, `from ${JSON.stringify(INDEX)}`));
    return (await import(pathToFileURL(out).href)).default;
  };
  const u = new URL(MYSQL!);
  const mysql2 = await driver("mysql2/promise").createConnection(own.url);
  const mariadb = await driver("mariadb").createConnection({ host: u.hostname, port: Number(u.port), user: decodeURIComponent(u.username) || "root", password: decodeURIComponent(u.password), database: own.url.split("/").pop() });
  try {
    const columns = await built("columns", "select name, code, legacy, body, old_body, price, part, at, span from sized order by id");
    for (const client of [mysql2, mariadb]) {
      const rows = await columns.run(client);
      assert.deepEqual(rows.map((r: { code: string | null }) => r.code), ["abcde", "ab", null]);
      assert.equal([...rows[0].name].length, 20);
      assert.equal(rows[0].old_body.length, 65535);
      assert.deepEqual(rows.map((r: { price: string | null }) => r.price), ["99999999.99", "-99999999.99", null]);
    }
    // A session that keeps a CHAR's padding gets it to exactly its length.
    await mysql2.query("set session sql_mode = concat(@@session.sql_mode, ',PAD_CHAR_TO_FULL_LENGTH')");
    assert.equal((await columns.run(mysql2))[1].code, "ab   ");
  } finally {
    await mysql2.end();
    await mariadb.end();
    for (const session of Session.opened()) await session.close();
    await own.drop();
  }
});
