import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { CLICKHOUSE_CODES } from "../src/error-codes-clickhouse.ts";
import { MYSQL_CODES } from "../src/error-codes-mysql.ts";
import { CLICKHOUSE_INFERRED, CLICKHOUSE_PRIVILEGE, CLICKHOUSE_SERVER, keptError, MYSQL_PRIVILEGE, MYSQL_SERVER, statementError } from "../src/server-errors.ts";

const rust = readFileSync(new URL("../engine/native/src/server_errors.rs", import.meta.url), "utf8");

/** A Rust list's `(number, "NAME")` entries, or its `(first, last)` runs, by the constant's name. */
function list(name: string): string {
  const body = new RegExp(`const ${name}: [^=]+= &\\[([^\\]]*)\\];`).exec(rust)?.[1];
  assert.ok(body !== undefined, `server_errors.rs has no ${name}`);
  return body;
}
const named = (name: string) => [...list(name).matchAll(/\((\d+), "(\w+)"\)/g)].map(([, n, s]) => [Number(n), s!] as const);
const runs = (name: string) => [...list(name).matchAll(/\((\d+), (\d+)\)/g)].map(([, a, b]) => [Number(a), Number(b)] as const);

test("the TypeScript lists of errors about the server's state and about privileges are the engine's, each name at its number", () => {
  for (const [rustName, codes, ours] of [["MYSQL_SERVER", MYSQL_CODES, MYSQL_SERVER], ["CLICKHOUSE_SERVER", CLICKHOUSE_CODES, CLICKHOUSE_SERVER], ["MYSQL_PRIVILEGE", MYSQL_CODES, MYSQL_PRIVILEGE], ["CLICKHOUSE_PRIVILEGE", CLICKHOUSE_CODES, CLICKHOUSE_PRIVILEGE], ["CLICKHOUSE_INFERRED", CLICKHOUSE_CODES, CLICKHOUSE_INFERRED]] as const) {
    const numbers = new Map<string, number>(codes.flatMap(([first, names]) => names.map((name, i) => [name, first + i] as const)));
    const listed = named(rustName);
    assert.deepEqual(new Set(listed.map(([, s]) => s)), ours, rustName);
    for (const [n, s] of listed) assert.equal(numbers.get(s), n, `${rustName}: ${s}`);
  }
});

test("the engine's runs of known numbers are the run time's tables", () => {
  assert.deepEqual(runs("MYSQL_NUMBERS"), MYSQL_CODES.map(([first, names]) => [first, first + names.length - 1]));
  assert.deepEqual(runs("CLICKHOUSE_NUMBERS"), CLICKHOUSE_CODES.map(([first, names]) => [first, first + names.length - 1]));
});

test("an error is the statement's by its database's code, and one no list knows is not", () => {
  const cases: [Parameters<typeof statementError>, boolean][] = [
    [["postgres", "42703"], true],
    [["postgres", "22P02"], true],
    [["postgres", "40P01"], false],
    [["postgres", "53200"], false],
    [["postgres", "XX000"], false],
    [["postgres", "42P05"], false],
    [["postgres", "55000"], true],
    [["postgres", "55006"], false],
    [["postgres", "26000"], true],
    [["postgres", undefined], false],
    [["mysql", "1064"], true],
    [["mysql", "1295"], true],
    [["mysql", "1213"], false],
    [["mysql", "1461"], false],
    [["mysql", "1815"], false],
    [["mysql", "1105", "HY000", "VT12001: unsupported: subquery in GROUP BY"], true],
    [["mysql", "1105", "HY000", "target: shop.-.primary: no healthy tablet available for 'keyspace:\"shop\"'"], false],
    [["mysql", "1105", "HY000", "operation not allowed in state NOT_SERVING"], false],
    [["mysql", "6999", "42000"], true],
    [["mysql", "6999", "HY000"], false],
    [["mysql", "6999"], false],
    [["clickhouse", "47"], true],
    [["clickhouse", "394"], false],
    [["clickhouse", "241"], false],
    [["clickhouse", "1000000"], false],
    [["sqlite", "SQLITE_ERROR"], true],
    [["sqlite", "SQLITE_AUTH"], true],
    [["sqlite", "SQLITE_BUSY"], false],
    [["sqlite", "SQLITE_IOERR_READ"], false],
    [["sqlite", "SQLITE_AUTH_USER"], false],
  ];
  for (const [args, expected] of cases) assert.equal(statementError(...args), expected, JSON.stringify(args));
});

test("an analysis refused for a privilege is not kept, nor one the server refused for its own reason", () => {
  for (const [dialect, code, kept] of [["postgres", "42703", true], ["postgres", "42501", false], ["postgres", "40P01", false], ["mysql", "1054", true], ["mysql", "1142", false], ["mysql", "1045", false], ["mysql", "1213", false], ["clickhouse", "47", true], ["clickhouse", "497", false], ["clickhouse", "394", false], ["clickhouse", "636", false], ["sqlite", "SQLITE_ERROR", true]] as const) {
    assert.equal(keptError(dialect, code), kept, `${dialect} ${code}`);
  }
});
