import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, realpathSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { Session } from "../src/analyze.ts";
import { sqlModule } from "../src/loader.ts";
import { defineQuery } from "../src/index.ts";
import { canonical, parseType, typeName } from "../src/clickhouse-types.ts";
import { internals } from "../src/decode-clickhouse.ts";
import { CLICKHOUSE, DRIVERS, INDEX, ROOT, clickhouseDatabase, clickhouseLive, declarations, driver, handWritten, transform } from "./helpers.ts";

// ClickHouse end to end, on the throwaway server REYLO_TEST_CLICKHOUSE names, each test in a
// database of its own: the types the content mapper writes for a statement, read from the server's
// own description of it, and the rows its module's `.run()` returns on `@clickhouse/client` and
// `@clickhouse/client-web` (lab/drivers), one representation per type (src/values.ts), each held
// to the server's own text of the value.

function project(url: string): string {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "reylo-clickhouse-")));
  writeFileSync(join(root, "package.json"), `{ "type": "module" }`);
  writeFileSync(join(root, ".env"), `DATABASE_URL=${url}\n`);
  mkdirSync(join(root, "node_modules"), { recursive: true });
  symlinkSync(ROOT, join(root, "node_modules", "reylo"));
  return root;
}

/** The module a build step writes for `sql` in `root`, imported. */
async function built(root: string, name: string, sql: string) {
  const file = join(root, `${name}.sql`);
  writeFileSync(file, sql);
  try {
    const { code } = await sqlModule(file, sql);
    const out = join(root, `${name}.mjs`);
    writeFileSync(out, code.replace(`from "reylo"`, `from ${JSON.stringify(INDEX)}`));
    return (await import(pathToFileURL(out).href)).default;
  } finally {
    for (const session of Session.opened()) await session.close();
  }
}

/** The `Params` and `Row` a declaration exports. */
function types(text: string): { params: string; row: string } {
  const params = /^export type Params = ([\s\S]*?);\nexport type Row/m.exec(text)![1]!;
  const row = /^export type Row = ([\s\S]*?);\n(?=\/\*\*|export )/m.exec(text)![1]!;
  return { params, row };
}

/** How many rows a declaration says the statement returns: its query's cardinality. */
const returns = (text: string): string => /^declare const query: Query<[\s\S]*?, "(one|maybeOne|many|none)", "clickhouse", QueryError>/m.exec(text)?.[1] ?? "";

/** Each diagnostic of a file, as its message and the text it marks. */
const marked = (sql: string, diagnostics: { messageText: string; start: number; length: number }[]) => diagnostics.map((d) => [d.messageText, sql.slice(d.start, d.start + d.length)]);

/** A query written by hand, with no build step: its statement, and each placeholder's place in it as the engine's lexer finds it (tests/helpers.ts, `handWritten`). */
const byHand = (sql: string, params?: string[]) => defineQuery<Record<string, unknown>, Record<string, unknown>, "many", "clickhouse">({ dialect: "clickhouse", ...handWritten(sql, "clickhouse", params) });

/** Every type the run time reads, each with a value that tries its representation: the SQL that makes it, and the value `.run()` gives. */
const VALUES: [name: string, sql: string, value: unknown][] = [
  ["u8", "toUInt8(255)", 255],
  ["i8", "toInt8(-128)", -128],
  ["u16", "toUInt16(65535)", 65535],
  ["i32", "toInt32(-2147483648)", -2147483648],
  ["u32", "toUInt32(4294967295)", 4294967295],
  ["u64", "toUInt64('18446744073709551615')", 18446744073709551615n],
  ["i64", "toInt64('-9223372036854775808')", -9223372036854775808n],
  ["i128", "toInt128('-170141183460469231731687303715884105728')", -170141183460469231731687303715884105728n],
  ["u128", "toUInt128('340282366920938463463374607431768211455')", 340282366920938463463374607431768211455n],
  ["u256", "toUInt256('115792089237316195423570985008687907853269984665640564039457584007913129639935')", 2n ** 256n - 1n],
  ["i256", "toInt256('-5')", -5n],
  ["f64", "toFloat64('0.1') + toFloat64('0.2')", 0.30000000000000004],
  ["f32", "toFloat32('0.1')", Math.fround(0.1)],
  ["f_nan", "nan", NaN],
  ["f_inf", "-inf", -Infinity],
  ["bf16", "CAST(1.5, 'BFloat16')", 1.5],
  ["dec32", "toDecimal32('-1.50', 2)", "-1.5"],
  ["dec64", "toDecimal64('0.05', 4)", "0.05"],
  ["dec128", "toDecimal128('12345678901234567890.1234567890', 10)", "12345678901234567890.123456789"],
  ["dec256", "toDecimal256('-0.000000000000000000000000000001', 30)", "-0.000000000000000000000000000001"],
  ["dec_zero", "toDecimal64('0', 4)", "0"],
  ["flag", "true", true],
  ["str", "'héllo\\0\\'x'", "héllo\0'x"],
  ["fixed", "toFixedString('ab', 4)", "ab\0\0"],
  ["uuid", "toUUID('00112233-4455-6677-8899-aabbccddeeff')", "00112233-4455-6677-8899-aabbccddeeff"],
  ["ip4", "toIPv4('1.2.3.4')", "1.2.3.4"],
  ["ip6", "toIPv6('2001:db8::1')", "2001:db8::1"],
  ["day", "toDate('2149-06-06')", "2149-06-06"],
  ["day32", "toDate32('1900-01-01')", "1900-01-01"],
  ["at", "toDateTime('2026-01-01 21:00:00', 'Asia/Tokyo')", "2026-01-01T12:00:00Z"],
  ["at9", "toDateTime64('2026-01-01 12:34:56.123456789', 9, 'UTC')", "2026-01-01T12:34:56.123456789Z"],
  ["before", "toDateTime64('1969-12-31 23:59:58.5', 3, 'UTC')", "1969-12-31T23:59:58.500Z"],
  ["at0", "toDateTime64('2026-01-01 00:00:00', 0, 'UTC')", "2026-01-01T00:00:00Z"],
  // A count of ticks ClickHouse's own text gives up on is still the instant it counts to, its year written as ISO 8601 extends one.
  ["far", "reinterpret(toInt64(3093496444800), 'DateTime64(0, \\'UTC\\')')", "+099999-01-01T00:00:00Z"],
  ["ancient", "reinterpret(toInt64(-70000000000), 'DateTime64(0, \\'UTC\\')')", "-000249-10-15T19:33:20Z"],
  ["farthest", "reinterpret(toInt64('9223372036854775807'), 'DateTime64(0, \\'UTC\\')')", "+292277026596-12-04T15:30:07Z"],
  ["earliest", "reinterpret(toInt64('-9223372036854775808'), 'DateTime64(3, \\'UTC\\')')", "-292275055-05-16T16:47:04.192Z"],
  ["role", "CAST('member', 'Enum8(\\'admin\\' = 1, \\'member\\' = -2)')", "member"],
  ["wide_role", "CAST('b', 'Enum16(\\'a\\' = 1, \\'b\\' = 300)')", "b"],
  ["list", "[toUInt16(1), 2]", [1, 2]],
  ["holes", "[1, NULL, 3]", [1, null, 3]],
  ["none", "emptyArrayString()", []],
  ["nested_list", "[[toUInt64(1)], []]", [[1n], []]],
  ["pair", "(toUInt8(1), 'a')", [1, "a"]],
  ["named", "CAST((1, 'a'), 'Tuple(n UInt8, s String)')", { n: 1, s: "a" }],
  ["twice", "map('k', toUInt8(1), 'k', 2)", [{ key: "k", value: 1 }, { key: "k", value: 2 }]],
  ["absent", "CAST(NULL, 'Nullable(UInt8)')", null],
  ["present", "toNullable(toUInt8(5))", 5],
  ["packed", "toLowCardinality('x')", "x"],
  ["packed_null", "CAST(NULL, 'LowCardinality(Nullable(String))')", null],
  ["either_number", "CAST(7, 'Variant(String, UInt8)')", 7],
  ["either_text", "CAST('a', 'Variant(String, UInt8)')", "a"],
  ["either_null", "CAST(NULL, 'Variant(String, UInt8)')", null],
  ["spot", "CAST((1.5, 2.5), 'Point')", [1.5, 2.5]],
  ["summed", "CAST(1, 'SimpleAggregateFunction(sum, UInt64)')", 1n],
];

/** Each parameter type the run time writes: the type, a value of it, and the value it comes back as where that differs. */
const PARAMS: [type: string, name: string, value: unknown, back?: unknown][] = [
  ["UInt8", "u8", 255],
  ["Int64", "i64", -9223372036854775808n],
  ["UInt64", "u64", 18446744073709551615n],
  ["UInt64", "u64_number", 7, 7n],
  ["UInt256", "u256", 2n ** 256n - 1n],
  ["Float64", "f", 0.1],
  ["Float64", "f_nan", NaN],
  ["Float64", "f_inf", -Infinity],
  ["Decimal(38, 10)", "dec", "12345678901234567890.123456789"],
  ["Bool", "flag", true],
  ["String", "s", "a\tb\nc\\N'd\0é\r"],
  ["String", "s_null_text", "\\N"],
  ["Nullable(String)", "s_null", null],
  ["FixedString(4)", "fixed", "ab", "ab\0\0"],
  ["UUID", "uuid", "61f0c404-5cb3-11e7-907b-a6006ad3dba0"],
  ["IPv4", "ip4", "1.2.3.4"],
  ["IPv6", "ip6", "2001:db8::1"],
  ["Enum8('a' = 1, 'it\\'s' = 2)", "label", "it's"],
  ["Date", "day", "2026-01-02"],
  ["Date32", "day32", "1900-01-01"],
  ["DateTime('Asia/Tokyo')", "at", "2026-01-01T12:00:00Z"],
  ["DateTime", "at_offset", "2026-01-01T21:00:00+09:00", "2026-01-01T12:00:00Z"],
  ["DateTime64(9, 'UTC')", "at9", "2026-01-01T12:34:56.123456789Z"],
  ["DateTime64(3)", "before", "1969-12-31T23:59:58.500Z"],
  ["DateTime64(3, 'UTC')", "at_short", "2026-01-01T00:00:00Z", "2026-01-01T00:00:00.000Z"],
  // ClickHouse reads a number of under five digits as a year, so the first hours of 1970 go with zeros before them.
  ["DateTime('UTC')", "epoch", "1970-01-01T00:00:00Z"],
  ["DateTime('UTC')", "epoch_9s", "1970-01-01T00:00:09Z"],
  ["DateTime64(0, 'UTC')", "epoch_5s", "1970-01-01T00:00:05Z"],
  ["DateTime64(0)", "before_5s", "1969-12-31T23:59:55Z"],
  ["DateTime64(3, 'UTC')", "at_1000s", "1970-01-01T00:16:40.000Z"],
  ["DateTime64(3, 'Asia/Tokyo')", "tokyo_before", "1969-12-31T23:59:58.750Z"],
  ["DateTime64(3, 'Asia/Tokyo')", "tokyo_after", "1970-01-01T00:00:00.250Z"],
  // It reads -0.5 as half a second after 1970, so the second before goes as its date and time, to a type that names UTC.
  ["DateTime64(3, 'UTC')", "half_before", "1969-12-31T23:59:59.500Z"],
  ["DateTime64(6, 'Etc/UTC')", "micro_before", "1969-12-31T23:59:59.999999Z"],
  ["Nullable(DateTime64(1, 'UTC'))", "tenth_before", "1969-12-31T23:59:59.9Z"],
  ["DateTime64(3, 'UTC')", "first", "1900-01-01T00:00:00.000Z"],
  ["DateTime64(6, 'UTC')", "last", "2299-12-31T23:59:59.999999Z"],
  // A 32-bit float goes as the shortest text that reads back as it, and comes back as the number it holds.
  ["Float32", "f32", 0.5],
  ["Float32", "f32_tenth", 0.1, Math.fround(0.1)],
  ["Array(Float32)", "f32s", [1 / 3, -16777217], [Math.fround(1 / 3), -16777216]],
  ["Array(String)", "strings", ["it's", "b\tc\\", ""]],
  ["Array(Nullable(UInt8))", "holes", [1, null]],
  ["Array(Array(UInt64))", "nested_list", [[1n], []]],
  // A tuple's and a map's own value is an array: the type says each level, at any depth.
  ["Array(Array(Array(UInt8)))", "deep_list", [[[1, 2]], [[3], []]]],
  ["Array(Array(Tuple(UInt8, String)))", "deep_pairs", [[[1, "a"]], [[2, "b"], [3, "c"]]]],
  ["Array(Array(Array(Tuple(n UInt8, s String))))", "deep_named", [[[{ n: 1, s: "a" }]]]],
  ["Array(Array(Map(String, UInt8)))", "deep_maps", [[[{ key: "k", value: 1 }], []]]],
  ["Array(Decimal(18, 2))", "decimals", ["1.5", "-2.25"]],
  ["Array(DateTime64(3, 'UTC'))", "instants", ["2026-01-01T00:00:00.500Z"]],
  ["Array(DateTime('UTC'))", "seconds", ["2026-01-01T00:00:00Z"]],
  ["Array(DateTime64(3, 'UTC'))", "instants_near", ["1969-12-31T23:59:59.500Z", "1970-01-01T00:00:00.000Z", "1970-01-01T00:16:40.000Z", "1969-12-31T23:43:20.250Z"]],
  ["Array(DateTime('UTC'))", "seconds_near", ["1970-01-01T00:00:00Z", "1970-01-01T00:00:05Z"]],
  ["Map(String, DateTime64(2, 'UTC'))", "map_before", [{ key: "a", value: "1969-12-31T23:59:59.99Z" }]],
  ["Tuple(DateTime('UTC'), DateTime64(0, 'UTC'))", "tuple_near", ["1970-01-01T00:00:07Z", "1970-01-01T00:00:05Z"]],
  ["Array(Date)", "days", ["2026-01-02"]],
  ["Array(UUID)", "uuids", ["61f0c404-5cb3-11e7-907b-a6006ad3dba0"]],
  ["Array(Bool)", "flags", [true, false]],
  ["Array(Float64)", "floats", [NaN, Infinity, -0]],
  ["Tuple(UInt64, String)", "pair", [1n, "a'b"]],
  ["Tuple(n UInt8, s Nullable(String))", "named", { n: 1, s: null }],
  ["Array(Tuple(UInt64, String))", "pairs", [[1n, "a"], [2n, "b"]]],
  ["Map(String, Array(UInt8))", "twice", [{ key: "k", value: [1, 2] }, { key: "it's", value: [] }]],
  ["LowCardinality(String)", "packed", "x"],
  ["LowCardinality(Nullable(String))", "packed_null", null],
];

/** Rows compared by value, a NaN equal to a NaN and -0 apart from 0, as `deepStrictEqual` compares them. */
const row = (pairs: [string, unknown][]) => Object.fromEntries(pairs);

test("a ClickHouse type's name reads into its parts and writes back on one line", () => {
  for (const name of ["UInt64", "Nullable(String)", "Decimal(18, 4)", "DateTime64(3, 'UTC')", "Enum8('admin' = 1, 'it\\'s' = -2)", "Tuple(n UInt8, s Nullable(String))", "Tuple(`a b` UInt8)", "Tuple(`a.b` UInt8, b String)", "Nested(`a.b` UInt8, `select` String)", "Tuple(`NULL` UInt8, `Values` String)", "Tuple(`a\\nb` UInt8)", "Enum8('a\\nb' = 1, 'c\\td' = 2, 'e\\rf' = 3, 'g\\0h' = 4, 'i\\bj' = 5, 'k\\fl' = 6)", "Tuple()", "Map(String, Array(UInt8))", "Variant(String, UInt8)", "AggregateFunction(quantiles(0.5, 0.9), UInt64)", "JSON(`a.b` UInt32, max_dynamic_paths = 8, SKIP c.d)"]) {
    assert.equal(typeName(parseType(name)!), name);
  }
  // As a describe writes a tuple with names, a member a line, and as a result's header writes it.
  assert.equal(canonical("Tuple(\n    x Float64,\n    y Tuple(\n        z String))"), "Tuple(x Float64, y Tuple(z String))");
  assert.equal(canonical("Decimal(18,4)"), "Decimal(18, 4)");
  // A module built before member names with a dot were quoted holds them bare, and a result naming the same members is not changed.
  assert.equal(canonical("Tuple(a.1 UInt8, a..b String, a. UInt8)"), canonical("Tuple(`a.1` UInt8, `a..b` String, `a.` UInt8)"));
  assert.notEqual(canonical("Tuple(a.2 UInt8)"), canonical("Tuple(`a.1` UInt8)"));
  assert.equal(parseType("Array(String"), undefined);
  assert.equal(parseType("UInt64 extra"), undefined);
});

test("ClickHouse: every type comes back as its one representation, on the Node client and the web client", { skip: clickhouseLive, timeout: 120_000 }, async () => {
  const sql = `SELECT ${VALUES.map(([name, value]) => `${value} AS ${name}`).join(", ")}`;
  const wanted = row(VALUES.map(([name, , value]) => [name, value]));
  for (const pkg of ["@clickhouse/client", "@clickhouse/client-web"]) {
    const client = driver(pkg).createClient({ url: CLICKHOUSE! });
    try {
      assert.deepEqual(await byHand(sql).run(client, {}), [wanted], pkg);
    } finally {
      await client.close();
    }
  }
});

test("ClickHouse: a value's text is the server's own", { skip: clickhouseLive, timeout: 120_000 }, async () => {
  const client = driver("@clickhouse/client").createClient({ url: CLICKHOUSE! });
  try {
    // An address in every form ClickHouse writes one, each beside the server's text of it.
    const addresses = ["::", "::1", "::2", "2001:db8::1", "::ffff:1.2.3.4", "::1.2.3.4", "::1:0", "::0.0.1.0", "::ffff", "64:ff9b::1.2.3.4", "::fffe:1.2.3.4", "2001:db8:0:1:1:1:1:1", "1::", "1:0:0:2:0:0:0:3", "1::2:0:0", "0:0:1::", "1:2:3:4:5:6:7:8", "ff02::1:2"];
    const [ips] = await byHand(`SELECT ${addresses.map((a, i) => `toIPv6('${a}') AS a${i}, toString(toIPv6('${a}')) AS s${i}`).join(", ")}`).run(client, {});
    addresses.forEach((a, i) => assert.equal(ips![`a${i}`], ips![`s${i}`], a));
    // A decimal at each width, and a date and an instant at their ends.
    const numbers = ["toDecimal32('99999.9999', 4)", "toDecimal32('-0.0001', 4)", "toDecimal64('-999999999999.999999', 6)", "toDecimal128('0.10', 2)", "toDecimal256('1' , 0)", "toDecimal64('100', 2)"];
    const [decimals] = await byHand(`SELECT ${numbers.map((n, i) => `${n} AS d${i}, toString(${n}) AS s${i}`).join(", ")}`).run(client, {});
    numbers.forEach((n, i) => assert.equal(decimals![`d${i}`], decimals![`s${i}`], n));
    const [ends] = await byHand("SELECT toDate('1970-01-01') AS d0, toDate32('2299-12-31') AS d1, toDateTime('2106-02-07 06:28:15', 'UTC') AS t0, toDateTime64('1900-01-01 00:00:00', 6, 'UTC') AS t1, toDateTime64('2262-04-11 23:47:16.854775807', 9, 'UTC') AS t2").run(client, {});
    assert.deepEqual(ends, { d0: "1970-01-01", d1: "2299-12-31", t0: "2106-02-07T06:28:15Z", t1: "1900-01-01T00:00:00.000000Z", t2: "2262-04-11T23:47:16.854775807Z" });
  } finally {
    await client.close();
  }
});

test("ClickHouse: bytes that are no UTF-8 text are refused by their column, never returned changed", { skip: clickhouseLive, timeout: 120_000 }, async () => {
  const client = driver("@clickhouse/client").createClient({ url: CLICKHOUSE! });
  try {
    await assert.rejects(byHand("SELECT unhex('ff00') AS raw").run(client, {}), /^TypeError: Column "raw" holds bytes that are not UTF-8 text, which a string cannot return as they are: select them as text, as hex\(raw\) or base64Encode\(raw\) does\.$/);
    // A digest in a FixedString, and a value inside an array.
    await assert.rejects(byHand("SELECT MD5('a') AS digest").run(client, {}), /Column "digest" holds bytes that are not UTF-8 text/);
    await assert.rejects(byHand("SELECT ['ok', unhex('c328')] AS list").run(client, {}), /Column "list" holds bytes that are not UTF-8 text/);
    // The same bytes as text, and text of every width, come back as they are.
    assert.deepEqual(await byHand("SELECT hex(MD5('a')) AS digest, 'h\u00e9llo \u2713 \ud83d\ude00' AS text, toFixedString('ab', 4) AS padded").run(client, {}), [{ digest: "0CC175B9C0F1B6A831C399E269772661", text: "h\u00e9llo \u2713 \ud83d\ude00", padded: "ab\0\0" }]);
  } finally {
    await client.close();
  }
});

test("ClickHouse: a session's setting that adds to a describe or filters a result is read past", { skip: clickhouseLive, timeout: 120_000 }, async () => {
  const db = await clickhouseDatabase("reylo_clickhouse_session", SCHEMA);
  try {
    // describe_include_subcolumns lists each tuple's element, array's sizes and nullable's flag after the columns.
    const shapes = { "shapes.sql": "SELECT tuple(1, 'a') AS t, [1] AS arr, CAST(NULL, 'Nullable(UInt8)') AS n" };
    const plain = types((await declarations(project(db.url), shapes))["shapes.sql"]!).row;
    assert.match(plain, /^\{ t: .+; arr: .+; n: .+ \}$/);
    assert.equal(types((await declarations(project(`${db.url}?ch_describe_include_subcolumns=1`), shapes))["shapes.sql"]!).row, plain);
    // A filter every result passes may drop the one row an aggregate returns.
    const counted = { "count.sql": "SELECT count() AS n FROM users" };
    assert.equal(returns((await declarations(project(db.url), counted))["count.sql"]!), "one");
    assert.equal(returns((await declarations(project(`${db.url}?ch_additional_result_filter=1`), counted))["count.sql"]!), "maybeOne");
  } finally {
    await db.drop();
  }
});

test("ClickHouse: each parameter is written as its type's text, and comes back as given", { skip: clickhouseLive, timeout: 120_000 }, async () => {
  const client = driver("@clickhouse/client").createClient({ url: CLICKHOUSE! });
  try {
    const sql = `SELECT ${PARAMS.map(([type, name]) => `{${name}:${type}} AS ${name}`).join(", ")}`;
    const given = row(PARAMS.map(([, name, value]) => [name, value]));
    const wanted = row(PARAMS.map(([, name, value, back]) => [name, back === undefined ? value : back]));
    assert.deepEqual(await byHand(sql, PARAMS.map(([, name]) => name)).run(client, given), [wanted]);
    // A parameter used twice is one value.
    assert.deepEqual(await byHand("SELECT {n:UInt8} + {n:UInt8} AS sum", ["n"]).run(client, { n: 2 }), [{ sum: 4 }]);
    // A query defined by hand with no marks cannot say what type each value goes as.
    const unmarked = defineQuery<Record<string, unknown>, Record<string, unknown>, "many", "clickhouse">({ dialect: "clickhouse", sql: "SELECT {n:UInt8} AS n", params: ["n"] });
    await assert.rejects(unmarked.run(client, { n: 2 }), /^TypeError: Reylo cannot bind this ClickHouse statement's parameters: each goes as its declared type's text, which Reylo reads at its placeholder, and a query defined by hand needs `marks` there/);
    assert.deepEqual(await defineQuery<Record<string, never>, Record<string, unknown>, "many", "clickhouse">({ dialect: "clickhouse", sql: "SELECT 1 AS n", params: [] }).run(client, {}), [{ n: 1 }]);
  } finally {
    await client.close();
  }
});

test("ClickHouse: a value its parameter's type cannot hold is refused before the statement runs", { skip: clickhouseLive, timeout: 120_000 }, async () => {
  const client = driver("@clickhouse/client").createClient({ url: CLICKHOUSE! });
  const refused = async (type: string, value: unknown) => {
    await assert.rejects(byHand(`SELECT {p:${type}} AS p`, ["p"]).run(client, { p: value }), (e: Error) => e.name === "TypeError" && /^Parameter "p"(?: at \S+)? has the ClickHouse type /.test(e.message), `${type} takes ${String(value)}`);
  };
  try {
    // ClickHouse wraps 300 into a UInt8 as 44, drops a decimal's digits past its scale, and reads a date outside its range as another.
    for (const [type, value] of [["UInt8", 300], ["UInt8", -1], ["UInt8", 1.5], ["Int8", "1"], ["Int64", 2n ** 63n], ["Decimal(18, 2)", "1.239"], ["Decimal(4, 2)", "100"], ["Date", "1960-01-01"], ["Date", "2026-02-30"], ["Date32", "2300-01-01"]] as const) await refused(type, value);
    // An instant names its zone, and keeps the digits its type has.
    for (const [type, value] of [["DateTime", "2026-01-01 12:00:00"], ["DateTime", "2026-01-01T12:00:00.5Z"], ["DateTime", "2107-01-01T00:00:00Z"], ["DateTime64(3)", "2026-01-01T12:00:00.1234Z"], ["DateTime64(3)", "2026-01-01T12:00:00 America/New_York"]] as const) await refused(type, value);
    // A date that is none of the calendar's would roll over to another, and an instant past 64 bits of nanoseconds wrap.
    for (const [type, value] of [["DateTime", "2026-02-30T00:00:00Z"], ["DateTime", "2026-13-01T00:00:00Z"], ["DateTime", "2026-01-01T24:00:00Z"], ["DateTime64(3)", "2026-01-01T23:59:60Z"], ["DateTime", "2026-01-01T00:00:00+99:99"], ["DateTime64(9)", "2262-04-12T00:00:00Z"]] as const) await refused(type, value);
    // A DateTime64 holds 1900 through 2299: some releases read an instant past them as the end of 9999.
    for (const [type, value] of [["DateTime64(3)", "2300-01-01T00:00:00Z"], ["DateTime64(3, 'UTC')", "+099999-01-01T00:00:00Z"], ["DateTime64(0)", "1899-12-31T23:59:59Z"], ["DateTime64(3)", "0001-01-01T00:00:00Z"]] as const) await refused(type, value);
    // ClickHouse reads -0.5 as half a second after 1970, and the second's date and time in the type's own zone.
    await assert.rejects(byHand("SELECT {p:DateTime64(3)} AS p", ["p"]).run(client, { p: "1969-12-31T23:59:59.500Z" }), /^TypeError: Parameter "p" has the ClickHouse type DateTime64\(3\), which ClickHouse reads an instant within the second before 1970 for only where the type names UTC, as DateTime64\(3, 'UTC'\) does; got "1969-12-31T23:59:59.500Z"\.$/);
    await refused("Array(DateTime64(6, 'Asia/Tokyo'))", ["1969-12-31T23:59:59.999999Z"]);
    // A number past 2^53 - 1 is not the digits written, a float past 32 bits is an infinity there, and half a surrogate pair goes as U+FFFD.
    for (const [type, value] of [["UInt64", 2 ** 60], ["Int128", -(2 ** 53)], ["Float32", 1e40], ["Float32", -3.5e38], ["BFloat16", 1e39], ["String", "\ud800"], ["FixedString(8)", "x\udfffy"], ["Array(String)", ["a\ud83d"]]] as const) await refused(type, value);
    // A member under another name would go as NULL, and one too many as nothing.
    for (const [type, value] of [["Tuple(a UInt8, note Nullable(String))", { a: 1, Note: "x" }], ["Tuple(a UInt8, note Nullable(String))", { a: 1 }], ["Tuple(a UInt8)", { a: 1, b: 2 }], ["Map(String, UInt8)", [{ key: "k", value: 1, extra: 2 }]]] as const) await refused(type, value);
    for (const [type, value] of [["FixedString(2)", "abc"], ["Enum8('a' = 1)", "b"], ["String", 1], ["String", null], ["Array(UInt8)", [1, null]], ["Tuple(UInt8, UInt8)", [1]], ["UUID", "nope"], ["Bool", 1], ["Map(String, UInt8)", { k: 1 }]] as const) await refused(type, value);
    await assert.rejects(byHand("SELECT {p:JSON} AS p", ["p"]).run(client, { p: {} }), /Parameter "p" has the ClickHouse type JSON, which Reylo does not write/);
    // The place inside the parameter is named.
    await assert.rejects(byHand("SELECT {p:Array(Tuple(n UInt8, s String))} AS p", ["p"]).run(client, { p: [{ n: 1, s: "a" }, { n: 300, s: "b" }] }), /^TypeError: Parameter "p" at \[1\]\.n has the ClickHouse type UInt8, which holds 0 to 255; got 300\.$/);
    // A hole in an array would go as nothing between two commas, which the server reads as a zero
    // or drops: it is the null it reads as everywhere else, refused where the type takes none.
    for (const [type, value] of [["Array(UInt8)", [1, , 3]], ["Array(Decimal(9, 2))", new Array(2)], ["Map(String, UInt8)", [{ key: "k", value: 1 }, ,]]] as const) await refused(type, value);
    assert.deepEqual(await byHand("SELECT {p:Array(Nullable(UInt8))} AS p", ["p"]).run(client, { p: [1, , 3] }), [{ p: [1, null, 3] }]);
    // The zero a number too small for a 32-bit float rounds to keeps its sign.
    assert.deepEqual((await byHand("SELECT {p:Float32} AS p", ["p"]).run(client, { p: -1e-82 })).map((row) => Object.is(row.p, -0)), [true]);
  } finally {
    await client.close();
  }
});

test("ClickHouse: an error is the server's, whether the statement fails at once or after its first rows", { skip: clickhouseLive, timeout: 120_000 }, async () => {
  const client = driver("@clickhouse/client").createClient({ url: CLICKHOUSE!, log: { level: 127 } });
  try {
    const atOnce = await byHand("SELECT nope FROM system.one").try(client, {});
    assert.equal(atOnce.error?.name, "QueryError");
    assert.deepEqual([atOnce.error?.code, atOnce.error?.number], ["UNKNOWN_IDENTIFIER", 47]);
    assert.match(atOnce.error!.message, /^Unknown expression identifier `nope`/);
    // The server has sent rows by the time the hundred-thousandth fails, and the error ends the result.
    // Its message is the server's text, byte for byte, whatever characters it holds.
    const late = await byHand("SELECT throwIf(number = 100000, 'boom – déjà vu €') AS x FROM numbers(200000) SETTINGS max_block_size = 1000").try(client, {});
    assert.deepEqual([late.error?.code, late.error?.number], ["FUNCTION_THROW_IF_VALUE_IS_NON_ZERO", 395]);
    assert.match(late.error!.message, /^boom – déjà vu €/);
    // What `.run()` throws for it has the message, the number and the name the client's own error has for one that fails at once.
    await assert.rejects(byHand("SELECT throwIf(number = 100000, 'late') AS x FROM numbers(200000) SETTINGS max_block_size = 1000").run(client, {}), (e: Error & { code?: string; type?: string }) => /^late/.test(e.message) && !/Code: |\(version /.test(e.message) && e.code === "395" && e.type === "FUNCTION_THROW_IF_VALUE_IS_NON_ZERO");
    // A parameter goes as a field of the request, and the server takes 128 KiB of one: its error names neither the parameter nor the limit.
    const long = await byHand("SELECT length({p:String}) AS n", ["p"]).try(client, { p: "x".repeat(140_000) });
    assert.deepEqual([long.error?.code, long.error?.number], ["POCO_EXCEPTION", 1000]);
    assert.match(long.error!.message, /Field value too long: the parameter "p" is 140000 bytes as ClickHouse's text, and the server takes 131072 of one unless its http_max_field_value_size is raised\./);
    // A format the statement names itself outranks the one Reylo asks for, on a server that takes one there: 26.8 and later.
    await assert.rejects(byHand("SELECT 1 AS x SETTINGS default_format = 'JSONEachRow'").run(client, {}), /^TypeError: The statement returned its result in the format JSONEachRow, and Reylo reads RowBinaryWithNamesAndTypes: take the format out|^Error: Setting default_format is neither a builtin setting|^Error: Unknown setting 'default_format'/);
    // A session that writes each type as bytes gives no name to read.
    const bytes = driver("@clickhouse/client").createClient({ url: CLICKHOUSE!, clickhouse_settings: { output_format_binary_encode_types_in_binary_format: 1 } });
    try {
      await assert.rejects(byHand("SELECT 1 AS x").run(bytes, {}), /whose name Reylo cannot read\. Where the session's setting output_format_binary_encode_types_in_binary_format is 1/);
    } finally {
      await bytes.close();
    }
    // Anything that is no client of ClickHouse's is refused by what one is.
    await assert.rejects(byHand("SELECT 1 AS n").run({ query() {} } as never, {}), /a client of @clickhouse\/client or @clickhouse\/client-web/);
  } finally {
    await client.close();
  }
});

const SCHEMA = `CREATE TABLE users (id UInt64, name String, email Nullable(String), age UInt8, balance Decimal(18, 4), created DateTime64(3, 'UTC'), role Enum8('admin' = 1, 'member' = 2), tags Array(String), meta Map(String, UInt32), spot Tuple(x Float64, y Float64), plan LowCardinality(String)) ENGINE = MergeTree ORDER BY id;
CREATE TABLE orders (id UInt64, user_id UInt64, total Decimal(18, 2), note Nullable(String)) ENGINE = MergeTree ORDER BY id SETTINGS enable_block_number_column = 1, enable_block_offset_column = 1;
INSERT INTO users VALUES (1, 'ada', NULL, 36, 1.5, '2026-01-02 03:04:05.678', 'admin', ['a', 'b'], map('logins', 3), (1.5, 2.5), 'pro'), (2, 'bob', 'bob@example.com', 41, 2.25, '2026-01-03 00:00:00', 'member', [], map(), (0, 0), 'free');
INSERT INTO orders VALUES (10, 1, 9.99, 'first')`;

test("ClickHouse: a select is typed from the server's own description, nullability and nested shapes included", { skip: clickhouseLive, timeout: 120_000 }, async () => {
  const db = await clickhouseDatabase("reylo_clickhouse_types", SCHEMA);
  try {
    const root = project(db.url);
    const files = {
      "user.sql": "SELECT * FROM users WHERE id = {id:UInt64} LIMIT 1",
      // An unmatched row of a left join is its type's default on ClickHouse, not NULL, and the server says so.
      "joined.sql": "SELECT u.id, o.total, o.note, groupArray((o.id, o.total)) OVER () AS all FROM users u LEFT JOIN orders o ON o.user_id = u.id ORDER BY u.id",
      "counted.sql": "SELECT count() AS users, max(age) AS oldest, maxOrNull(email) AS last_email FROM users WHERE id > {after:UInt64}",
      "names.sql": "SELECT name FROM users WHERE name LIKE {pattern:String} AND created >= {since:DateTime64(3, 'UTC')} AND id IN {ids:Array(UInt64)} LIMIT {count:UInt64}",
      "total.sql": "SELECT sum(balance) FROM users",
      // ClickHouse types an IN of constants a UInt8, and of a column a Nullable(UInt8): a parameter is a constant as the statement runs.
      "found.sql": "SELECT id, id IN {ids:Array(UInt64)} AS found, id IN (7, {other:UInt64}) AS either FROM users ORDER BY id",
      // A format and an aggregate's parameter are constants to ClickHouse, and neither decides a type.
      "shown.sql": "SELECT name, formatDateTime(created, {format:String}) AS shown FROM users WHERE id = {id:UInt64}",
      "middle.sql": "SELECT quantile({level:Float64})(age) AS middle, count() AS n FROM users",
      // A function of a LowCardinality column and a constant is LowCardinality, and the statement is described with a text its toDateTime reads.
      "labelled.sql": "SELECT concat(plan, {suffix:String}) AS label, plan = {plan:String} AS on_plan FROM users WHERE created >= toDateTime({since:String}) ORDER BY id",
      // A string a statement reads as a time, a zone or a unit is a constant to ClickHouse, and decides no type.
      "since.sql": "SELECT id FROM users WHERE created >= {since:String} ORDER BY id",
      "by_day.sql": "SELECT toDate(created, {zone:String}) AS day, count() AS n FROM users GROUP BY day ORDER BY day",
      // One column with no name is the statement's value, whatever ClickHouse calls it: its name holds the parameter's value.
      "more.sql": "SELECT max(id) + {more:UInt32} FROM users",
      // The `:` of a conditional may stand before a name and a cast, which is no parameter.
      "either.sql": "SELECT id > 1 ? name :name::String AS v FROM users ORDER BY id",
      // A Variant is whichever of its types its value has, each as a column of that type is: an
      // array, a tuple or a map among them, and an integer a setting lets stand beside a wider one.
      "variant.sql": "SELECT CAST([1, 2], 'Variant(Array(UInt8), String)') AS list, CAST(CAST((1, NULL), 'Tuple(n UInt8, s Nullable(String))'), 'Variant(Tuple(n UInt8, s Nullable(String)), UInt64)') AS named, CAST((toUInt8(1), 'a'), 'Variant(Tuple(UInt8, String), UInt64)') AS pair, CAST(map('k', toUInt8(1)), 'Variant(Map(String, UInt8), String)') AS pairs, [CAST('x', 'Variant(Array(UInt8), String)')] AS listed, CAST(toUInt8(1), 'Variant(UInt8, UInt64)') AS small SETTINGS allow_suspicious_variant_types = 1",
    };
    const out = await declarations(root, files);
    assert.deepEqual(types(out["user.sql"]!), {
      params: "{ id: number | bigint }",
      row: '{ id: bigint; name: string; email: string | null; age: number; balance: string; created: string; role: "admin" | "member"; tags: string[]; meta: { key: string; value: number }[]; spot: { x: number; y: number }; plan: string }',
    });
    assert.equal(returns(out["user.sql"]!), "maybeOne");
    assert.equal(types(out["joined.sql"]!).row, "{ id: bigint; total: string; note: string | null; all: [bigint, string][] }");
    assert.equal(returns(out["joined.sql"]!), "many");
    assert.deepEqual(types(out["counted.sql"]!), { params: "{ after: number | bigint }", row: "{ users: bigint; oldest: number; last_email: string | null }" });
    assert.equal(returns(out["counted.sql"]!), "one");
    assert.equal(types(out["names.sql"]!).params, "{ pattern: string; since: string; ids: (number | bigint)[]; count: number | bigint }");
    assert.equal(types(out["found.sql"]!).row, "{ id: bigint; found: number; either: number }");
    assert.deepEqual(types(out["shown.sql"]!), { params: "{ format: string; id: number | bigint }", row: "{ name: string; shown: string }" });
    assert.equal(types(out["middle.sql"]!).row, "{ middle: number; n: bigint }");
    assert.equal(returns(out["middle.sql"]!), "one");
    assert.deepEqual(types(out["labelled.sql"]!), { params: "{ suffix: string; plan: string; since: string }", row: "{ label: string; on_plan: number }" });
    assert.deepEqual(types(out["since.sql"]!), { params: "{ since: string }", row: "{ id: bigint }" });
    assert.deepEqual(types(out["by_day.sql"]!), { params: "{ zone: string }", row: "{ day: string; n: bigint }" });
    assert.match(out["more.sql"]!, /^declare const query: Query<\{ more: number \}, bigint, "one", "clickhouse", QueryError>/m);
    assert.deepEqual(types(out["either.sql"]!), { params: "Record<never, never>", row: "{ v: string }" });
    assert.equal(types(out["variant.sql"]!).row, "{ list: string | number[] | null; named: bigint | { n: number; s: string | null } | null; pair: bigint | [number, string] | null; pairs: string | { key: string; value: number }[] | null; listed: (string | number[] | null)[]; small: bigint | number | null }");
    // One column with no name of its own is the statement's value.
    assert.match(out["total.sql"]!, /^declare const query: Query<Record<never, never>, string, "one", "clickhouse", QueryError>/m);

    const user = await built(root, "user", files["user.sql"]);
    assert.deepEqual(await user.run(db.client, { id: 1 }), { id: 1n, name: "ada", email: null, age: 36, balance: "1.5", created: "2026-01-02T03:04:05.678Z", role: "admin", tags: ["a", "b"], meta: [{ key: "logins", value: 3 }], spot: { x: 1.5, y: 2.5 }, plan: "pro" });
    assert.equal(await user.run(db.client, { id: 3n }), null);
    const joined = await built(root, "joined", files["joined.sql"]);
    assert.deepEqual(await joined.run(db.client, {}), [
      { id: 1n, total: "9.99", note: "first", all: [[10n, "9.99"], [0n, "0"]] },
      { id: 2n, total: "0", note: null, all: [[10n, "9.99"], [0n, "0"]] },
    ]);
    const counted = await built(root, "counted", files["counted.sql"]);
    assert.deepEqual(await counted.run(db.client, { after: 0 }), { users: 2n, oldest: 41, last_email: "bob@example.com" });
    assert.deepEqual(await counted.run(db.client, { after: 9 }), { users: 0n, oldest: 0, last_email: null });
    const names = await built(root, "names", files["names.sql"]);
    assert.deepEqual(await names.run(db.client, { pattern: "%", since: "2026-01-02T03:04:05.678Z", ids: [1, 2n], count: 5 }), [{ name: "ada" }, { name: "bob" }]);
    assert.equal(await (await built(root, "total", files["total.sql"])).run(db.client, {}), "3.75");
    // Each runs with the types read for it, which the result's own are held against.
    assert.deepEqual(await (await built(root, "labelled", files["labelled.sql"])).run(db.client, { suffix: "!", plan: "pro", since: "2026-01-01 00:00:00" }), [{ label: "pro!", on_plan: 1 }, { label: "free!", on_plan: 0 }]);
    assert.deepEqual(await (await built(root, "since", files["since.sql"])).run(db.client, { since: "2026-01-03 00:00:00" }), [{ id: 2n }]);
    assert.deepEqual(await (await built(root, "by_day", files["by_day.sql"])).run(db.client, { zone: "Asia/Tokyo" }), [{ day: "2026-01-02", n: 1n }, { day: "2026-01-03", n: 1n }]);
    assert.equal(await (await built(root, "more", files["more.sql"])).run(db.client, { more: 5 }), 7n);
    assert.deepEqual(await (await built(root, "either", files["either.sql"])).run(db.client, {}), [{ v: "ada" }, { v: "bob" }]);
    assert.deepEqual(await (await built(root, "variant", files["variant.sql"])).run(db.client, {}), [{ list: [1, 2], named: { n: 1, s: null }, pair: [1, "a"], pairs: [{ key: "k", value: 1 }], listed: ["x"], small: 1 }]);
    const found = await built(root, "found", files["found.sql"]);
    assert.deepEqual(await found.run(db.client, { ids: [2n, 5n], other: 1 }), [{ id: 1n, found: 0, either: 1 }, { id: 2n, found: 1, either: 0 }]);
    const shown = await built(root, "shown", files["shown.sql"]);
    assert.deepEqual(await shown.run(db.client, { format: "%Y/%m", id: 1 }), [{ name: "ada", shown: "2026/01" }]);
    assert.deepEqual(await (await built(root, "middle", files["middle.sql"])).run(db.client, { level: 0.5 }), { middle: 38.5, n: 2n });
    // A Standard Query says what the statement is and maps no values on ClickHouse: a parameter
    // goes by its name, and a result comes as one body, which `.run()` reads.
    const standard = user["~standard"].query;
    assert.deepEqual([standard.dialect, standard.cardinality, standard.columns], ["clickhouse", "maybeOne", undefined]);
    assert.throws(() => standard.encode({ id: 1 }), /A Standard Query maps no values on ClickHouse, whose parameters are named in the statement/);
    assert.throws(() => standard.decode([[1n]]), /A Standard Query maps no values on ClickHouse/);
    assert.throws(() => standard.output([{ id: "1" }]), /A Standard Query maps no values on ClickHouse/);
    assert.doesNotMatch(out["user.sql"]!, /~standard/);
    // A query's JSON Schema is of what its parameters take: a wide whole number is a number or a
    // BigInt and no digits, of no named format past a signed 64 bits, a UInt64 among them, and an
    // array's element is null where its type says so.
    const schemas = await built(root, "schemas", "SELECT {ids:Array(Nullable(UInt8))} AS ids, {wide:UInt128} AS wide, {count:UInt64} AS count");
    assert.deepEqual(schemas.inputSchema.properties, { ids: { type: "array", items: { anyOf: [{ type: "integer", format: "int32" }, { type: "null" }] } }, wide: { type: "integer" }, count: { type: "integer" } });
    assert.deepEqual(schemas.outputSchema.items.properties, schemas.inputSchema.properties);
    assert.deepEqual(await schemas.run(db.client, { ids: [1, null], wide: 2n ** 100n, count: 3 }), [{ ids: [1, null], wide: 2n ** 100n, count: 3n }]);
    // A value that is always null admits null alone, which `type` names once.
    const nothing = await built(root, "nothing", "SELECT NULL AS n");
    assert.deepEqual(nothing.outputSchema.items.properties.n, { type: "null" });
    // The web client reads the same bytes.
    const web = driver("@clickhouse/client-web").createClient({ url: db.address });
    try {
      assert.deepEqual(await counted.run(web, { after: 1 }), { users: 1n, oldest: 41, last_email: "bob@example.com" });
    } finally {
      await web.close();
    }
  } finally {
    await db.drop();
  }
});

test("ClickHouse: the project's codecs reach the values inside a tuple, an array and a map, in a result and in a parameter", { skip: clickhouseLive, timeout: 120_000 }, async () => {
  const db = await clickhouseDatabase("reylo_clickhouse_codecs");
  try {
    const root = project(db.url);
    writeFileSync(join(root, ".reylo.yaml"), "js:\n  codecs:\n    decimal: number\n");
    // A Variant's value is read as its member's type alone, which no codec reaches.
    const sql = "SELECT CAST((toDecimal64('12.34', 2), 'a'), 'Tuple(price Decimal(12, 2), label String)') AS named, (toDecimal64('1.5', 2), toUInt8(7)) AS unnamed, [toDecimal64('2.25', 2)] AS list, map('k', toDecimal64('3.5', 1)) AS priced, [(toDecimal64('4.5', 1), 'x')] AS pairs, CAST(toDecimal64('5.5', 2), 'Variant(Array(UInt8), Decimal(18, 2))') AS held, {p:Tuple(Decimal(9, 2), Array(Decimal(9, 2)))} AS given, {q:Decimal(9, 2)} AS q";
    const out = await declarations(root, { "nested.sql": sql });
    assert.deepEqual(types(out["nested.sql"]!), {
      params: "{ p: [number, number[]]; q: number }",
      row: "{ named: { price: number; label: string }; unnamed: [number, number]; list: number[]; priced: { key: string; value: number }[]; pairs: [number, string][]; held: string | number[] | null; given: [number, number[]]; q: number }",
    });
    // Each value is the type its declaration says, and a parameter's goes back as its type's text.
    const nested = await built(root, "nested", sql);
    assert.deepEqual([await nested.run(db.client, { p: [1.25, [2.5]], q: 0.5 })].flat(), [{ named: { price: 12.34, label: "a" }, unnamed: [1.5, 7], list: [2.25], priced: [{ key: "k", value: 3.5 }], pairs: [[4.5, "x"]], held: "5.5", given: [1.25, [2.5]], q: 0.5 }]);
    await assert.rejects(nested.run(db.client, { p: ["1.25", []], q: 0.5 }), /takes a number, as js.codecs.decimal in the project file says/);
  } finally {
    await db.drop();
  }
});

test("ClickHouse: a result whose types are not the ones the type check read is refused", { skip: clickhouseLive, timeout: 120_000 }, async () => {
  const db = await clickhouseDatabase("reylo_clickhouse_drift", SCHEMA);
  try {
    const root = project(db.url);
    const joined = await built(root, "joined", "SELECT u.id, o.total FROM users u LEFT JOIN orders o ON o.user_id = u.id ORDER BY u.id");
    assert.deepEqual(await joined.run(db.client, {}), [{ id: 1n, total: "9.99" }, { id: 2n, total: "0" }]);
    // A client whose session nulls an unmatched row gets a Nullable column, which the type check did not read.
    const nulling = db.connect({ clickhouse_settings: { join_use_nulls: 1 } });
    try {
      await assert.rejects(joined.run(nulling, {}), /joined\.sql returned the column "total" as Nullable\(Decimal\(18, 2\)\) where the type check read Decimal\(18, 2\)\. The table changed after the type check, or the session that runs the statement has a setting the type check's session lacks, as join_use_nulls/);
    } finally {
      await nulling.close();
    }
    // The setting written in the statement is read by the type check and the run alike.
    const out = await declarations(root, { "nulled.sql": "SELECT u.id, o.total FROM users u LEFT JOIN orders o ON o.user_id = u.id ORDER BY u.id SETTINGS join_use_nulls = 1" });
    assert.equal(types(out["nulled.sql"]!).row, "{ id: bigint; total: string | null }");
    // A column whose type changed after the build is refused the same way.
    await db.client.command({ query: "ALTER TABLE orders MODIFY COLUMN total Decimal(18, 4)" });
    await assert.rejects(joined.run(db.client, {}), /returned the column "total" as Decimal\(18, 4\) where the type check read Decimal\(18, 2\)/);
  } finally {
    await db.drop();
  }
});

test("ClickHouse: a table of another database is typed again once it changes", { skip: clickhouseLive, timeout: 120_000 }, async () => {
  const db = await clickhouseDatabase("reylo_clickhouse_other");
  const other = `reylo_clickhouse_other_side_${process.pid}`;
  try {
    await db.client.command({ query: `DROP DATABASE IF EXISTS ${other}` });
    await db.client.command({ query: `CREATE DATABASE ${other}` });
    await db.client.command({ query: `CREATE TABLE ${other}.readings (id UInt64, level UInt8) ENGINE = MergeTree ORDER BY id` });
    // And a table of this database whose one name reads as the other's with its database's: the two are told apart.
    await db.client.command({ query: `CREATE TABLE \`${other}.readings\` (id UInt64, level String) ENGINE = MergeTree ORDER BY id` });
    const root = project(db.url);
    const files = { "levels.sql": `SELECT id, level FROM ${other}.readings`, "named.sql": `SELECT id, level FROM \`${other}.readings\`` };
    const first = await declarations(root, files);
    assert.deepEqual([types(first["levels.sql"]!).row, types(first["named.sql"]!).row], ["{ id: bigint; level: number }", "{ id: bigint; level: string }"]);
    // The cache's fingerprint covers the other database too, each table by its database and its name:
    // once the two change types with each other, a new session reads each one's new type.
    await db.client.command({ query: `ALTER TABLE ${other}.readings MODIFY COLUMN level String` });
    await db.client.command({ query: `ALTER TABLE \`${other}.readings\` MODIFY COLUMN level UInt8` });
    const second = await declarations(root, files);
    assert.deepEqual([types(second["levels.sql"]!).row, types(second["named.sql"]!).row], ["{ id: bigint; level: string }", "{ id: bigint; level: number }"]);
  } finally {
    await db.client.command({ query: `DROP DATABASE IF EXISTS ${other}` });
    await db.drop();
  }
});

test("ClickHouse: what cannot be typed is an error at its place", { skip: clickhouseLive, timeout: 120_000 }, async () => {
  const db = await clickhouseDatabase("reylo_clickhouse_refused", SCHEMA);
  try {
    const root = project(db.url);
    const files = {
      // The scale of a DateTime64 is read from the parameter's value, so the column has no one type.
      "scale.sql": "SELECT toDateTime64(created, {scale:UInt8}) AS t FROM users WHERE id = {id:UInt64}",
      "cast.sql": "SELECT CAST(age, {type:String}) AS a FROM users",
      "name.sql": "SELECT {column:Identifier} FROM users",
      "untyped.sql": "SELECT id FROM users WHERE id = :id",
      "unknown.sql": "SELECT id, nope FROM users",
      "syntax.sql": "SELECT id FROM users WHERE",
      "json.sql": "SELECT CAST('{\"a\": 1}', 'JSON') AS doc",
      "unnamed.sql": "SELECT id, age + 1 FROM users",
      "two.sql": "SELECT 1 AS a; SELECT 2 AS b",
      "ddl.sql": "CREATE TABLE t (id UInt64) ENGINE = MergeTree ORDER BY id",
      "variant.sql": "SELECT {v:Variant(String, UInt8)} AS v",
      // A place in a tuple past the first few, where the type changes.
      "place.sql": "SELECT tupleElement((1.5, 2.5, 3.5, 'x'), {at:UInt8}) AS e",
      "star.sql": "SELECT * FROM (SELECT id, id + {x:UInt32} FROM users)",
      "totals.sql": "SELECT plan, count() AS n FROM users GROUP BY plan WITH TOTALS",
      "format.sql": "SELECT id FROM users SETTINGS default_format = 'JSONEachRow'",
      "parsed.sql": "SELECT parseDateTime({at:String}, '%d/%m/%Y') AS t",
    };
    const found = await transform(root, files);
    const of = (name: keyof typeof files) => marked(files[name], found[name]!.diagnostics);
    assert.equal(of("scale.sql").length, 1);
    assert.equal(of("scale.sql")[0]![1], "{scale:UInt8}");
    assert.match(of("scale.sql")[0]![0]!, /^ClickHouse reads \{scale:UInt8\} as a constant here, and the result's types change with its value: column 1 \(t\) has the type DateTime64\(1.* with 1 and DateTime64\(2.* with 2\. Write the value in the statement\.$/);
    assert.equal(of("cast.sql")[0]![1], "{type:String}");
    assert.match(of("cast.sql")[0]![0]!, /^ClickHouse reads \{type:String\} as a constant here, and the statement fails with each value tried in its place, as with 'a', so its value is part of what the statement says\. Write the value in the statement\./);
    assert.match(of("name.sql")[0]![0]!, /^\{column:Identifier\} stands for a name/);
    assert.match(of("untyped.sql")[0]![0]!, /A ClickHouse parameter is written with its type: \{id:Type\}, or :id::Type\.$/);
    assert.equal(of("untyped.sql")[0]![1], ":");
    assert.match(of("unknown.sql")[0]![0]!, /^Unknown expression identifier `nope`/);
    assert.match(of("syntax.sql")[0]![0]!, /^Syntax error/);
    assert.match(of("json.sql")[0]![0]!, /^Column 1 \(doc\) has the type JSON, and Reylo does not read a ClickHouse JSON yet/);
    assert.match(of("unnamed.sql")[0]![0]!, /^Column 2 has no name: the database calls it "plus\(age, 1\)"/);
    assert.match(of("two.sql")[0]![0]!, /^A typed \.sql file holds one statement and this one holds several/);
    assert.match(of("ddl.sql")[0]![0]!, /^Reylo types a ClickHouse SELECT, INSERT, DELETE or UPDATE, and CREATE is none of them/);
    assert.match(of("variant.sql")[0]![0]!, /is a Variant, whose value's text does not say which of its types it is/);
    assert.deepEqual([of("place.sql").length, of("place.sql")[0]![1]], [1, "{at:UInt8}"]);
    assert.match(of("place.sql")[0]![0]!, /^ClickHouse reads \{at:UInt8\} as a constant here, and the result's types change with its value: column 1 \(e\) has the type Float64 with 1 and String with 4\. Write the value in the statement\.$/);
    assert.match(of("star.sql")[0]![0]!, /^Column 2 is named "plus\(id, \{x:UInt32\}\)" by its expression, which holds a parameter's value, so its name changes from run to run\. Give it one with `as` where the statement makes it\.$/);
    assert.match(of("totals.sql")[0]![0]!, /^WITH TOTALS returns its totals beside the rows, and a typed result is its rows alone/);
    assert.deepEqual(of("format.sql"), [["The statement's SETTINGS names a format for its result, default_format, and Reylo reads the one it asks for: take the setting out.", "default_format"]]);
    assert.equal(of("parsed.sql")[0]![1], "{at:String}");
    assert.match(of("parsed.sql")[0]![0]!, /^ClickHouse computes with \{at:String\} as it reads the statement, and the statement fails with each value tried in its place, as with 'a'\. Declare the parameter as the type the statement converts it to, or write the value in the statement\. \(/);
  } finally {
    await db.drop();
  }
});

test("ClickHouse: a write returns no rows, and is checked against its table", { skip: clickhouseLive, timeout: 120_000 }, async () => {
  const db = await clickhouseDatabase("reylo_clickhouse_writes", `${SCHEMA};\nCREATE TABLE things (id UInt64, format String, d Dynamic, v Variant(UInt64, String), tags Array(UInt8), items Nested(sku String, quantity UInt32)) ENGINE = MergeTree ORDER BY id`);
  // What the type check says of a parameter written as it is to a column that keeps another type.
  const unkept = (written: string, column: string, type: string, fix: string) => `${written} is written as it is to the column ${column}, of the type ${type}. ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error. ${fix}`;
  // And of a value the statement computes with a parameter, which has the type the server describes it with: at the
  // parameter, where the statement's text shows which one, and of the whole statement where it does not.
  const computed = (written: string | null, value: string, column: string, type: string, why = `ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error. Give each parameter the value is computed from a type the column keeps, or convert the value in the statement, as CAST(… AS ${type}) does.`) =>
    written === null ? `The statement writes a value of the type ${value} that depends on a parameter to the column ${column}, of the type ${type}. ${why}` : `${written} is in a value of the type ${value} that is written to the column ${column}, of the type ${type}. ${why}`;
  const WIDE = unkept("{total:Decimal(18, 4)}", "total", "Decimal(18, 2)", "Declare the parameter as the column's type, {total:Decimal(18, 2)}.");
  try {
    const root = project(db.url);
    const files = {
      "add.sql": "INSERT INTO orders (id, user_id, total, note) VALUES ({id:UInt64}, {user:UInt64}, {total:Decimal(18, 2)}, {note:Nullable(String)})",
      "copy.sql": "INSERT INTO orders SELECT id + {offset:UInt64}, user_id, total, note FROM orders",
      "remove.sql": "DELETE FROM orders WHERE id = {id:UInt64}",
      "bad_column.sql": "INSERT INTO orders (id, nope) VALUES ({id:UInt64}, 1)",
      "bad_where.sql": "DELETE FROM orders WHERE nope = {id:UInt64}",
      "from_body.sql": "INSERT INTO orders FORMAT JSONEachRow",
      // An UPDATE is checked as the SELECT of each column it writes and each value it computes.
      "set.sql": "UPDATE orders SET note = {note:Nullable(String)}, total = total + {more:Decimal(18, 2)} WHERE id = {id:UInt64}",
      "alter_set.sql": "ALTER TABLE orders UPDATE note = {note:Nullable(String)} WHERE id IN (SELECT id FROM orders WHERE total > {least:Decimal(18, 2)})",
      "set_bad.sql": "UPDATE orders SET nope = {note:String} WHERE id = {id:UInt64}",
      // An ALTER that changes no rows is a migration's.
      "alter_add.sql": "ALTER TABLE orders ADD COLUMN z UInt8",
      // ClickHouse fits what a statement inserts into the column, in silence: 1.2345 would go in as 1.23.
      "wide.sql": "INSERT INTO orders (id, user_id, total, note) VALUES ({id:UInt64}, {user:UInt32}, {total:Decimal(18, 4)}, {note:Nullable(String)})",
      // And what a SELECT's list or an assignment writes, the same way: as it is, or within what the statement computes.
      "copy_one.sql": "INSERT INTO orders (id, user_id, total, note) SELECT {id:UInt64}, {user:UInt64}, {total:Decimal(18, 2)} AS total, {note:Nullable(String)}",
      "copy_wide.sql": "INSERT INTO orders (id, user_id, total) SELECT {id:UInt64}, {user:UInt64}, {total:Decimal(18, 4)}",
      "set_wide.sql": "UPDATE orders SET total = {total:Decimal(18, 4)} WHERE id = {id:UInt64}",
      "alter_wide.sql": "ALTER TABLE orders UPDATE total = {total:Decimal(18, 4)} WHERE id = {id:UInt64}",
      "computed.sql": "INSERT INTO orders (id, user_id) VALUES ({id:UInt64}, {user:Int64} + 0)",
      "computed_set.sql": "UPDATE orders SET user_id = user_id + {more:Int64} WHERE id = {id:UInt64}",
      "computed_from.sql": "INSERT INTO orders (id, user_id) SELECT 14, wide FROM (SELECT {user:Int64} AS wide)",
      // A value the statement converts has the type it names, and a parameter that only keeps rows is in no value.
      "computed_cast.sql": "INSERT INTO orders (id, user_id) VALUES ({id:UInt64}, CAST({user:Int64} + 0, 'UInt64'))",
      "kept_rows.sql": "INSERT INTO things (id, format) SELECT id, name FROM users WHERE age > {age:UInt8} LIMIT {most:UInt64}",
      // A second in a finer clock: the wider type holds every value of the narrower.
      "kept_wider.sql": "INSERT INTO users (id, name, created) VALUES ({id:UInt64}, 'w', {created:DateTime})",
      // Each row of VALUES is checked as the SELECT of its values.
      "bad_value.sql": "INSERT INTO orders (id, user_id, total) VALUES ({id:UInt64}, 1, 0), ({id:UInt64}, nope + 1, 0)",
      "bad_count.sql": "INSERT INTO orders (id, user_id) VALUES ({id:UInt64}, 1, 2)",
      "bad_type.sql": "INSERT INTO orders (id, user_id) VALUES ({id:UInt64}, 'abc')",
      // A partition's expression is read with the condition, and several commands are no one statement's.
      "remove_in.sql": "DELETE FROM orders IN PARTITION tuple() WHERE id = {id:UInt64}",
      "remove_in_bad.sql": "DELETE FROM orders IN PARTITION tuple() WHERE nope = {id:UInt64}",
      "alter_two.sql": "ALTER TABLE orders UPDATE note = 'x' WHERE id = {id:UInt64}, DELETE WHERE id = 0",
      // What a write takes as it is has the column's own type: a text is no array and no enum's
      // label. A Dynamic holds a value of any type as that type.
      "text_array.sql": "INSERT INTO things (id, tags) VALUES ({id:UInt64}, {tags:String})",
      "text_role.sql": "INSERT INTO users (id, name, role) VALUES ({id:UInt64}, 'x', {role:String})",
      "dynamic.sql": "INSERT INTO things (id, d) SELECT {id:UInt64}, {d:UInt64}",
      // VALUES stores the text of a value for a Dynamic where it reads as data, a placeholder's own included.
      "dynamic_values.sql": "INSERT INTO things (id, d) VALUES ({id:UInt64}, ({d:UInt64} + 1))",
      // A text written to a Variant is read as whichever of its types the text reads as.
      "variant_text.sql": "INSERT INTO things (id, v) SELECT {id:UInt64}, {v:String}",
      // A converted value, one in parentheses, a member's type in a Variant, a column named as a clause is, and a Nested column's parts.
      "converted.sql": "INSERT INTO things (id, format, d, v, items.sku, items.quantity) SELECT {id:UInt64}, {format:String}, {d:UInt64}::Dynamic, ({v:UInt64}), {skus:Array(String)}, {quantities:Array(UInt32)}",
      // VALUES reads a value for a Variant by the column's type: a 5 is its UInt64.
      "variant_literal.sql": "INSERT INTO things (id, v) VALUES ({id:UInt64}, 5)",
      // What a call or a cast computes for a Dynamic is stored, in VALUES too.
      "cast_dynamic.sql": "INSERT INTO things (id, d) VALUES ({id:UInt64}, CAST({d:UInt64}, 'Dynamic'))",
      // A SELECT in parentheses is read as any other, and a partition's id is the statement's to write.
      "wrapped_wide.sql": "INSERT INTO orders (id, user_id) SELECT {id:UInt64}, {user:Int64} UNION ALL (SELECT 1, 2)",
      "wrapped.sql": "INSERT INTO orders (id, user_id) SELECT {id:UInt64}, 1 UNION ALL (SELECT 2, {user:UInt64})",
      "remove_part.sql": "DELETE FROM orders IN PARTITION ID {part:String} WHERE id = {id:UInt64}",
      // The server says which column a value after a `*` goes to, and what a `*` replaces.
      "starred.sql": "INSERT INTO orders SELECT * EXCEPT (note), {note:Nullable(String)} FROM orders",
      "starred_wide.sql": "INSERT INTO orders SELECT * REPLACE ({total:Decimal(18, 4)} AS total) FROM orders",
      // Where the text does not say which column a value goes to, nothing holds it.
      "unnamed.sql": "INSERT INTO FUNCTION null('id UInt8') SELECT {id:UInt64} + 0",
      // A column is the table's own, whatever alias the statement gives its name, and the settings an INSERT
      // names before its rows are the rows' too.
      "set_alias.sql": "ALTER TABLE orders UPDATE user_id = {user:Int64} WHERE (total AS user_id) > 0",
      "settings_first.sql": "INSERT INTO orders (id, user_id) SETTINGS async_insert = 1, wait_for_async_insert = 1 VALUES ({id:UInt64}, {user:UInt64} + 0)",
      "settings_type.sql": "INSERT INTO users (id, name, created) SETTINGS enable_extended_results_for_datetime_functions = 1 VALUES ({id:UInt64}, 's', toStartOfDay({at:DateTime64(6, 'UTC')}))",
      // Where ClickHouse reads a parameter as a constant, a value computed with one takes a conversion that names its type.
      "zoned.sql": "INSERT INTO users (id, name, created) VALUES ({id:UInt64}, 'z', toDateTime64({at:DateTime}, 3, {zone:String}))",
      "zoned_cast.sql": "INSERT INTO users (id, name, created) VALUES ({id:UInt64}, 'z', CAST(toDateTime64({at:DateTime}, 3, {zone:String}) AS DateTime64(3)))",
      // A map as VALUES alone reads one is written, and a value an UPDATE's column does not take is refused.
      "mapped.sql": "INSERT INTO users (id, name, meta) VALUES ({id:UInt64}, 'm', {'logins': 2})",
      "set_text.sql": "UPDATE orders SET user_id = 'abc' WHERE id = {id:UInt64}",
      // A text the statement converts to its column's type, as the docs' own example does.
      "placed.sql": "INSERT INTO users (id, name, created) VALUES ({id:UInt64}, 'p', {created:String}::DateTime64(3, 'UTC'))",
      // A sum is wider than what it adds: it stands where the column keeps the type of each thing it adds, and it is a number of the column's kind.
      "sum.sql": "UPDATE users SET age = age + {by:UInt8} WHERE id = {id:UInt64}",
      "sum_values.sql": "INSERT INTO users (id, name, age) VALUES ({id:UInt64}, 's', {a:UInt8} + {b:UInt8})",
      "sum_wide.sql": "INSERT INTO users (id, name, age) VALUES ({id:UInt64}, 's', {a:UInt8} + {b:UInt16})",
      // A part of a sum the server computes ahead stands for what it adds.
      "sum_folded.sql": "INSERT INTO users (id, name, age) VALUES ({id:UInt64}, 's', {a:UInt8} + (1 + 2))",
      // A quotient is a float, and a text is read: neither is a number of the column's kind.
      "quotient.sql": "INSERT INTO users (id, name, age) VALUES ({id:UInt64}, 's', {a:UInt8} / {b:UInt8})",
      "joined.sql": "INSERT INTO users (id, name, age) VALUES ({id:UInt64}, 's', concat(toString({a:UInt8}), toString({b:UInt8})))",
      "variant_sum.sql": "INSERT INTO things (id, v) VALUES ({id:UInt64}, {a:UInt64} - {b:UInt64})",
      // A value a parameter only chooses is no sum, and neither is one that adds a column the target does not keep.
      "chosen.sql": "INSERT INTO users (id, name, age) SELECT {id:UInt64}, 's', if({take:Bool}, id, 0) FROM users",
      "sum_column.sql": "UPDATE users SET age = id + {by:UInt8} WHERE id = {id:UInt64}",
      // A query that adds rows of its own gives a label's column values that are no result of the choice.
      "label_filled.sql": "INSERT INTO users (id, role) SELECT number, if({admin:Bool}, 'admin', 'member') FROM numbers(3) WHERE number != 1 ORDER BY number WITH FILL",
      // A query it reads from may add rows: the choice is computed for each.
      "label_filled_source.sql": "INSERT INTO users (id, role) SELECT number, if({admin:Bool}, 'admin', 'member') FROM (SELECT number FROM numbers(3) WHERE number != 1 ORDER BY number WITH FILL)",
      // A text that is one of an enum's labels whichever its parameters choose is the label.
      "label.sql": "INSERT INTO users (id, name, role) VALUES ({id:UInt64}, 'l', if({admin:Bool}, 'admin', 'member'))",
      "label_set.sql": "UPDATE users SET role = multiIf({level:UInt8} > 8, 'admin', 'member') WHERE id = {id:UInt64}",
      "label_text.sql": "INSERT INTO users (id, name, role) VALUES ({id:UInt64}, 'l', if({admin:Bool}, 'admin', '2'))",
      // A CASE chooses as an if does, and a result may be the column's own value.
      "label_case.sql": "INSERT INTO users (id, name, role) SELECT {id:UInt64}, 'l', CASE {level:UInt8} WHEN 9 THEN 'admin' ELSE 'member' END",
      "label_same.sql": "ALTER TABLE users UPDATE role = if({admin:Bool}, 'admin', role) WHERE id = {id:UInt64}",
      // A column that holds no NULL: an INSERT writes its default value in place of one, and an UPDATE fails.
      "null_add.sql": "INSERT INTO users (id, name) VALUES ({id:UInt64}, {name:Nullable(String)})",
      "null_set.sql": "UPDATE users SET name = {name:Nullable(String)} WHERE id = {id:UInt64}",
      "null_named.sql": "UPDATE users SET name = ifNull({name:Nullable(String)}, '') WHERE id = {id:UInt64}",
      // A value that may be NULL is held as one, whatever it is computed from.
      "null_computed.sql": "INSERT INTO users (id, name, age) VALUES ({id:UInt64}, 'n', nullIf({age:UInt8}, 0))",
    };
    const found = await transform(root, files);
    for (const name of ["add.sql", "copy.sql", "remove.sql", "set.sql", "alter_set.sql", "copy_one.sql", "remove_in.sql", "converted.sql", "mapped.sql", "placed.sql", "variant_literal.sql", "cast_dynamic.sql", "wrapped.sql", "starred.sql", "computed_cast.sql", "kept_rows.sql", "kept_wider.sql", "dynamic.sql", "settings_first.sql", "zoned_cast.sql", "sum.sql", "sum_values.sql", "label.sql", "label_set.sql", "label_case.sql", "label_same.sql", "null_named.sql", "sum_folded.sql", "label_filled_source.sql"] as const) assert.deepEqual(found[name]!.diagnostics, [], name);
    assert.deepEqual(marked(files["sum_wide.sql"], found["sum_wide.sql"]!.diagnostics), [["This value of the type UInt32 depends on a parameter and is written to the column age, of the type UInt8. ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error. Give each parameter the value is computed from a type the column keeps, or convert the value in the statement, as CAST(… AS UInt8) does.", "{a:UInt8} + {b:UInt16}"]]);
    const converted = (value: string, to: string) => `This value of the type ${value} depends on a parameter and is written to the column age, of the type UInt8. ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error. Convert the value in the statement, as CAST(… AS ${to}) does.`;
    assert.deepEqual(marked(files["quotient.sql"], found["quotient.sql"]!.diagnostics), [[converted("Float64", "UInt8"), "{a:UInt8} / {b:UInt8}"]]);
    assert.deepEqual(marked(files["joined.sql"], found["joined.sql"]!.diagnostics), [[converted("String", "UInt8"), "concat(toString({a:UInt8}), toString({b:UInt8}))"]]);
    assert.deepEqual(marked(files["variant_sum.sql"], found["variant_sum.sql"]!.diagnostics), [["This value of the type Int64 depends on a parameter and is written to the column v, of the type Variant(String, UInt64). ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error. Give the value one of the types the column holds, other than a text: declare its parameters so, or convert it in the statement.", "{a:UInt64} - {b:UInt64}"]]);
    assert.deepEqual(marked(files["chosen.sql"], found["chosen.sql"]!.diagnostics), [["The statement writes a value of the type UInt64 that depends on a parameter to the column age, of the type UInt8. ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error. Convert the value in the statement, as CAST(… AS UInt8) does.", files["chosen.sql"]]]);
    assert.deepEqual(marked(files["sum_column.sql"], found["sum_column.sql"]!.diagnostics), [[computed("{by:UInt8}", "UInt64", "age", "UInt8", "ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error. Convert the value in the statement, as CAST(… AS UInt8) does."), "{by:UInt8}"]]);
    assert.deepEqual(marked(files["label_filled.sql"], found["label_filled.sql"]!.diagnostics), [[computed("{admin:Bool}", "String", "role", "Enum8('admin' = 1, 'member' = 2)", "ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error. Convert the value in the statement, as CAST(… AS Enum8('admin' = 1, 'member' = 2)) does."), "{admin:Bool}"]]);
    // A parameter that could not be of the column's kind is none a declaration changes.
    assert.deepEqual(marked(files["label_text.sql"], found["label_text.sql"]!.diagnostics), [[computed("{admin:Bool}", "String", "role", "Enum8('admin' = 1, 'member' = 2)", "ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error. Convert the value in the statement, as CAST(… AS Enum8('admin' = 1, 'member' = 2)) does."), "{admin:Bool}"]]);
    assert.deepEqual(marked(files["null_computed.sql"], found["null_computed.sql"]!.diagnostics), [[computed("{age:UInt8}", "Nullable(UInt8)", "age", "UInt8", "The column holds no NULL, and an INSERT writes the column's default value in place of one, with no error unless a setting asks for one. Write what a NULL becomes in the statement, as ifNull(…, …) does."), "{age:UInt8}"]]);
    const never = "Declare the parameter as a type that is never NULL, {name:String}, or write what a NULL becomes in the statement, as ifNull({name:Nullable(String)}, …) does.";
    assert.deepEqual(marked(files["null_add.sql"], found["null_add.sql"]!.diagnostics), [[`{name:Nullable(String)} is written as it is to the column name, of the type String. The column holds no NULL, and an INSERT writes the column's default value in place of one, with no error unless a setting asks for one. ${never}`, "{name:Nullable(String)}"]]);
    assert.deepEqual(marked(files["null_set.sql"], found["null_set.sql"]!.diagnostics), [[`{name:Nullable(String)} is written as it is to the column name, of the type String. The column holds no NULL, and an UPDATE that writes one fails. ${never}`, "{name:Nullable(String)}"]]);
    assert.deepEqual(marked(files["text_array.sql"], found["text_array.sql"]!.diagnostics), [[unkept("{tags:String}", "tags", "Array(UInt8)", "Declare the parameter as the column's type, {tags:Array(UInt8)}."), "{tags:String}"]]);
    assert.deepEqual(marked(files["text_role.sql"], found["text_role.sql"]!.diagnostics), [[unkept("{role:String}", "role", "Enum8('admin' = 1, 'member' = 2)", "Declare the parameter as the column's type, {role:Enum8('admin' = 1, 'member' = 2)}."), "{role:String}"]]);
    assert.deepEqual(marked(files["set_alias.sql"], found["set_alias.sql"]!.diagnostics), [[unkept("{user:Int64}", "user_id", "UInt64", "Declare the parameter as the column's type, {user:UInt64}."), "{user:Int64}"]]);
    assert.deepEqual(marked(files["settings_type.sql"], found["settings_type.sql"]!.diagnostics), [[computed("{at:DateTime64(6, 'UTC')}", "DateTime64(6, 'UTC')", "created", "DateTime64(3, 'UTC')"), "{at:DateTime64(6, 'UTC')}"]]);
    assert.deepEqual(marked(files["zoned.sql"], found["zoned.sql"]!.diagnostics), [["This value depends on a parameter and is written to the column created, of the type DateTime64(3, 'UTC'). ClickHouse reads a parameter of this statement as a constant, and the type of a value computed with one may change with the parameter's value. Give the value one type in the statement, as CAST(… AS DateTime64(3, 'UTC')) does.", "toDateTime64({at:DateTime}, 3, {zone:String})"]]);
    assert.deepEqual(marked(files["dynamic_values.sql"], found["dynamic_values.sql"]!.diagnostics), [["{d:UInt64} is in a value that VALUES writes to the column d, of the type Dynamic. ClickHouse's VALUES reads a value for a Dynamic by its text where the value is a parameter alone or opens with a parenthesis or a bracket, and stores the placeholder's own text in place of the parameter's value. Write the row as an INSERT … SELECT, which computes each value.", "{d:UInt64}"]]);
    assert.deepEqual(marked(files["variant_text.sql"], found["variant_text.sql"]!.diagnostics), [[computed(null, "String", "v", "Variant(String, UInt64)", "ClickHouse reads a text written to a Variant as whichever of its types the text reads as, so the column would not hold the text as it was given: 123 is stored as a number where the Variant has one."), files["variant_text.sql"]]]);
    assert.deepEqual(marked(files["starred_wide.sql"], found["starred_wide.sql"]!.diagnostics), [[computed("{total:Decimal(18, 4)}", "Decimal(18, 4)", "total", "Decimal(18, 2)"), "{total:Decimal(18, 4)}"]]);
    assert.deepEqual(marked(files["unnamed.sql"], found["unnamed.sql"]!.diagnostics), [["{id:UInt64} is in a value that is written to a table function's column, and Reylo reads no table function's columns, so nothing holds it to that column's type. Insert into a table.", "{id:UInt64}"]]);
    assert.deepEqual(marked(files["computed.sql"], found["computed.sql"]!.diagnostics), [[computed("{user:Int64}", "Int64", "user_id", "UInt64"), "{user:Int64}"]]);
    assert.deepEqual(marked(files["computed_set.sql"], found["computed_set.sql"]!.diagnostics), [[computed("{more:Int64}", "Int64", "user_id", "UInt64"), "{more:Int64}"]]);
    assert.deepEqual(marked(files["computed_from.sql"], found["computed_from.sql"]!.diagnostics), [[computed("{user:Int64}", "Int64", "user_id", "UInt64"), "{user:Int64}"]]);
    assert.deepEqual(marked(files["remove_part.sql"], found["remove_part.sql"]!.diagnostics), [["Reylo types no parameter as a partition's id, which the server reads from the statement's text where a write is checked. Write the id in the statement in place of {part:String}, or name the partition by its key's value, as IN PARTITION {day:Date} does.", "{part:String}"]]);
    assert.deepEqual(marked(files["wrapped_wide.sql"], found["wrapped_wide.sql"]!.diagnostics), [[computed(null, "Int64", "user_id", "UInt64"), files["wrapped_wide.sql"]]]);
    assert.match(found["set_text.sql"]!.diagnostics[0]!.messageText, /^Cannot parse string 'abc' as UInt64/);
    for (const name of ["set_wide.sql", "alter_wide.sql"] as const) assert.deepEqual(marked(files[name], found[name]!.diagnostics), [[WIDE, "{total:Decimal(18, 4)}"]], name);
    assert.deepEqual(marked(files["copy_wide.sql"], found["copy_wide.sql"]!.diagnostics), [[computed(null, "Decimal(18, 4)", "total", "Decimal(18, 2)"), files["copy_wide.sql"]]]);
    assert.match(found["bad_value.sql"]!.diagnostics[0]!.messageText, /^Unknown expression or function identifier `nope`/);
    assert.equal(marked(files["bad_value.sql"], found["bad_value.sql"]!.diagnostics)[0]![1], "({id:UInt64}, nope + 1, 0)");
    assert.match(found["bad_count.sql"]!.diagnostics[0]!.messageText, /^Number of columns doesn't match/);
    assert.match(found["bad_type.sql"]!.diagnostics[0]!.messageText, /^Cannot parse string 'abc' as UInt64/);
    assert.match(found["remove_in_bad.sql"]!.diagnostics[0]!.messageText, /^Unknown expression or function identifier `nope`/);
    assert.match(found["alter_two.sql"]!.diagnostics[0]!.messageText, /^Reylo types an ALTER TABLE of one command, an UPDATE or a DELETE, and this one has 2/);
    assert.match(found["set_bad.sql"]!.diagnostics[0]!.messageText, /`nope`/);
    assert.match(found["alter_add.sql"]!.diagnostics[0]!.messageText, /^Reylo types an ALTER TABLE that changes rows, with UPDATE or DELETE, and ADD COLUMN is neither/);
    assert.deepEqual(marked(files["wide.sql"], found["wide.sql"]!.diagnostics), [[WIDE, "{total:Decimal(18, 4)}"]]);
    assert.equal(types(found["add.sql"]!.text).params, "{ id: number | bigint; user: number | bigint; total: string; note?: string | null }");
    assert.equal(returns(found["add.sql"]!.text), "none");
    assert.match(found["bad_column.sql"]!.diagnostics[0]!.messageText, /^No such column nope in table/);
    assert.match(found["bad_where.sql"]!.diagnostics[0]!.messageText, /^Unknown expression or function identifier `nope`/);
    assert.match(found["from_body.sql"]!.diagnostics[0]!.messageText, /^An INSERT … FORMAT takes its rows from the request's body/);
    // A parameter written with a cast after its name is sent as ClickHouse's own, `{name:Type}`.
    const cast = await built(root, "cast", "SELECT :n::UInt8 + 1 AS next, :s::Nullable(String) AS s");
    assert.equal(cast.sql, "SELECT {n:UInt8}::UInt8 + 1 AS next, {s:Nullable(String)}::Nullable(String) AS s");
    assert.deepEqual(await cast.run(db.client, { n: 1, s: null }), [{ next: 2, s: null }]);
    const add = await built(root, "add", files["add.sql"]);
    // The module sends its statement with the comment naming its file, which an INSERT's rows must not run into.
    // A parameter that takes NULL may be left out, and is NULL then.
    assert.equal(await add.run(db.client, { id: 11, user: 2, total: "5.25" }), undefined);
    await assert.rejects(add.run(db.client, { id: 12, total: "1" } as never), /Parameter "user" is missing, and the statement takes no null for it/);
    const read = byHand("SELECT id, total, note FROM orders ORDER BY id");
    assert.deepEqual(await read.run(db.client, {}), [{ id: 10n, total: "9.99", note: "first" }, { id: 11n, total: "5.25", note: null }]);
    // What a SELECT's list writes is what was given: the row comes back as it went in.
    const copy = await built(root, "copy_one", files["copy_one.sql"]);
    await copy.run(db.client, { id: 12, user: 2, total: "0.01", note: "copied" });
    assert.deepEqual((await read.run(db.client, {})).at(-1), { id: 12n, total: "0.01", note: "copied" });
    await (await built(root, "remove_in", files["remove_in.sql"])).run(db.client, { id: 12 });
    assert.deepEqual(await read.run(db.client, {}), [{ id: 10n, total: "9.99", note: "first" }, { id: 11n, total: "5.25", note: null }]);
    const remove = await built(root, "remove", files["remove.sql"]);
    await remove.run(db.client, { id: 10 });
    assert.deepEqual(await read.run(db.client, {}), [{ id: 11n, total: "5.25", note: null }]);
    // What the statement converts, and what the column's own type takes, is what the table holds.
    await (await built(root, "converted", files["converted.sql"])).run(db.client, { id: 20, format: "csv", d: 300, v: 300, skus: ["a"], quantities: [2] });
    assert.deepEqual(await byHand("SELECT format, toString(d) AS held, dynamicType(d) AS kind, v.UInt64 AS v, items.sku AS skus, items.quantity AS quantities FROM things").run(db.client, {}), [{ format: "csv", held: "300", kind: "UInt64", v: 300n, skus: ["a"], quantities: [2] }]);
    await (await built(root, "variant_literal", files["variant_literal.sql"])).run(db.client, { id: 21 });
    assert.deepEqual(await byHand("SELECT v.UInt64 AS v FROM things WHERE id = 21").run(db.client, {}), [{ v: 5n }]);
    await (await built(root, "cast_dynamic", files["cast_dynamic.sql"])).run(db.client, { id: 22, d: 7 });
    assert.deepEqual(await byHand("SELECT toString(d) AS held, dynamicType(d) AS kind FROM things WHERE id = 22").run(db.client, {}), [{ held: "7", kind: "UInt64" }]);
    await (await built(root, "mapped", files["mapped.sql"])).run(db.client, { id: 30 });
    assert.deepEqual(await byHand("SELECT meta FROM users WHERE id = 30").run(db.client, {}), [{ meta: [{ key: "logins", value: 2 }] }]);
    await (await built(root, "placed", files["placed.sql"])).run(db.client, { id: 40, created: "2026-01-02 03:04:05.678" });
    assert.deepEqual(await byHand("SELECT created FROM users WHERE id = 40").run(db.client, {}), [{ created: "2026-01-02T03:04:05.678Z" }]);
    // A label the statement chooses is the label the table holds, and a sum of what the column keeps is the sum.
    const label = await built(root, "label", files["label.sql"]);
    await label.run(db.client, { id: 50, admin: true });
    await label.run(db.client, { id: 51, admin: false });
    await (await built(root, "sum_values", files["sum_values.sql"])).run(db.client, { id: 52, a: 3, b: 4 });
    assert.deepEqual(await byHand("SELECT id, role, age FROM users WHERE id >= 50 ORDER BY id").run(db.client, {}), [{ id: 50n, role: "admin", age: 0 }, { id: 51n, role: "member", age: 0 }, { id: 52n, role: "admin", age: 7 }]);
    // An UPDATE runs where its table keeps the two block columns a lightweight update needs, as this one does.
    const set = await built(root, "set", files["set.sql"]);
    assert.equal(await set.run(db.client, { id: 11, note: "paid", more: "1.5" }), undefined);
    assert.deepEqual(await read.run(db.client, {}), [{ id: 11n, total: "6.75", note: "paid" }]);
  } finally {
    await db.drop();
  }
});

test("ClickHouse: an error that ends a result is read whatever its length, and a result with no mark for one is refused", async () => {
  const bytes = (text: string) => Uint8Array.from(text, (c) => c.charCodeAt(0));
  // One Bool column, `p`, and three rows of it.
  const rows = "\x01\x01p\x04Bool\x01\x00\x01";
  const ended = (message: string, tag: string) => bytes(`${rows}__exception__\r\n${tag}\r\n${message}\n${message.length} ${tag}\r\n__exception__\r\n`);
  // A message longer than any window read from the body's end is still the error, and no row.
  for (const length of [40, 70_000]) {
    const error = internals.midStreamError(ended(`Code: 395. DB::Exception: ${"x".repeat(length)}. (FUNCTION_THROW_IF_VALUE_IS_NON_ZERO) (version 26.8.1.1 (official build))`, "tag"), "tag") as Error & { code?: string; type?: string };
    assert.deepEqual([error.message, error.code, error.type], [`${"x".repeat(length)}.`, "395", "FUNCTION_THROW_IF_VALUE_IS_NON_ZERO"]);
  }
  // Rows alone are no error, and neither is a body that ends with another response's mark.
  assert.equal(internals.midStreamError(bytes(rows), "tag"), undefined);
  assert.equal(internals.midStreamError(ended("Code: 1. DB::Exception: no. (UNSUPPORTED_METHOD)", "other"), "tag"), undefined);
  // A server before 25.11 sends no mark, and its error after the rows would read as more of them.
  const answer = (headers: Record<string, string>) => {
    const call = async () => ({ stream: (async function* () { yield bytes(`${rows}Code: 395. DB::Exception: late. (FUNCTION_THROW_IF_VALUE_IS_NON_ZERO) (version 25.8.1.1)\n`); })(), response_headers: headers });
    return { exec: call, query: call, insert: call, command: call };
  };
  const query = defineQuery<Record<string, never>, Record<string, unknown>, "many", "clickhouse">({ dialect: "clickhouse", sql: "SELECT throwIf(number = 2) AS p FROM numbers(3)", params: [] });
  await assert.rejects(query.run(answer({}) as never, {}), /^TypeError: The statement's result came without the X-ClickHouse-Exception-Tag header, which ClickHouse sends from 25\.11 on/);
  await assert.rejects(query.run(answer({ "x-clickhouse-exception-tag": "" }) as never, {}), /came without the X-ClickHouse-Exception-Tag header/);
  // A query defined by hand that declares one name with two types has no one text to send for it.
  const twice = defineQuery<Record<string, unknown>, Record<string, unknown>, "many", "clickhouse">({ dialect: "clickhouse", sql: "SELECT {x:UInt8} AS a, {x:String} AS b", params: ["x"], marks: [[7, 16], [23, 33]] });
  await assert.rejects(twice.run(answer({ "x-clickhouse-exception-tag": "tag" }) as never, { x: 300 }), /^TypeError: Parameter "x" is declared as UInt8 and as String in the statement: a ClickHouse parameter has one type\.$/);
  // A name in the result's header that is no UTF-8 would read as another's.
  const named = { exec: async () => ({ stream: (async function* () { yield bytes("\x01\x01p\x17Enum8('\xff' = 1, '\xfe' = 2)\x01"); })(), response_headers: { "x-clickhouse-exception-tag": "tag" } }) };
  await assert.rejects(query.run({ ...answer({}), ...named } as never, {}), /^TypeError: The result's type for the column "p" holds bytes that are not UTF-8 text/);
});
