import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { FINGERPRINT } from "../src/analyze.ts";

// Each catalog an engine queries is one its dialect's fingerprint reads, or one whose every change
// moves a row the fingerprint does read, named here with why: a catalog the engine starts reading
// otherwise would leave an entry current after a change to it.
const catalogs = {
  mysql: {
    sources: ["engine/mysql/src", "engine/native/src/mysql.rs"],
    pattern: /information_schema\.(\w+)/gi,
    named: (name: string) => `information_schema.${name}`,
    covered: {
      tables: "a table's name and kind, which the columns and views rows carry",
      view_table_usage: "what a view's definition names, which the views row hashes",
      view_routine_usage: "what a view's definition names, which the views row hashes",
      collations: "the server's own, which its version carries",
    } as Record<string, string>,
  },
  clickhouse: {
    sources: ["engine/clickhouse/src", "engine/native/src/clickhouse.rs"],
    pattern: /system\.(\w+)/gi,
    named: (name: string) => `system.${name}`,
    covered: { one: "a constant table of one row" } as Record<string, string>,
  },
} as const;

for (const [dialect, { sources, pattern, named, covered }] of Object.entries(catalogs)) {
  test(`every catalog the ${dialect} engine queries moves its fingerprint`, () => {
    const files = sources.flatMap((source) => {
      const path = fileURLToPath(new URL(`../${source}`, import.meta.url));
      return statSync(path).isDirectory() ? readdirSync(path).filter((name) => name.endsWith(".rs")).map((name) => join(path, name)) : [path];
    });
    const read = new Set(files.flatMap((file) => [...readFileSync(file, "utf8").matchAll(pattern)].map((m) => m[1]!.toLowerCase())));
    assert.ok(read.size > 0);
    const fingerprint = FINGERPRINT[dialect as keyof typeof FINGERPRINT].toLowerCase();
    const missed = [...read].filter((name) => !fingerprint.includes(named(name)) && !(name in covered)).sort();
    assert.deepEqual(missed, [], `the ${dialect} engine queries ${missed.join(", ")}, which FINGERPRINT.${dialect} in src/analyze.ts does not read: add what an analysis reads there to it`);
  });
}
