import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { moduleCode, moduleImports, type ModuleInput } from "../src/loader.ts";
import { withBases, type EngineType } from "../src/engine-type.ts";
import { array, field, pg, record } from "./descriptors.ts";

// Statements' analyses as the engine gave them, each with the project's settings, and the module a
// build step wrote for it, held character for character: Postgres with a decimal codec and its
// library, a JSON column that may hold NaN, a lone value, a structured insert, and a total order
// with nested arrays and no sqlcommenter comment; MySQL's structured insert with the lists it
// writes again, and a cast beside a typed JSON parameter; SQLite.
const FIXTURES: { name: string; input: ModuleInput; code: string }[] = JSON.parse(readFileSync(new URL("./module-code.json", import.meta.url), "utf8"));

test("a module is its imports, its exports and its query's definition, each field written as it was", () => {
  assert.equal(FIXTURES.length, 8);
  for (const { name, input, code } of FIXTURES) assert.equal(moduleCode(input), code, name);
});

test("a Postgres module imports its rarer types where a column or a parameter holds one, a structured parameter's money included, and nowhere else", () => {
  const template = FIXTURES.find((f) => f.name.startsWith("postgres"))!.input;
  const typed = (columns: EngineType[], params: { type: EngineType; members?: { written: string }[] }[] = []) =>
    moduleImports({ ...template, codecs: {}, analysis: { ...template.analysis, columns: columns.map((type, i) => ({ name: `c${i}`, type })), params: params.map((p, i) => ({ name: `p${i}`, ...p })) } as ModuleInput["analysis"] }).typed;
  for (const name of ["point", "box", "int4range", "int4multirange", "money", "hstore"]) {
    assert.equal(typed([pg(name)]), true, name);
    assert.equal(typed([array(pg(name))]), true, `${name}[]`);
    assert.equal(typed([], [{ type: pg(name) }]), true, `a ${name} parameter`);
  }
  assert.equal(typed([record([field("a", pg("int4"))])]), true, "a row value");
  for (const name of ["int4", "text", "timestamptz", "interval", "jsonb", "bytea", "uuid", "numeric"]) assert.equal(typed([pg(name)], [{ type: pg(name) }]), false, name);
  // An array of a domain over an array type is read without them, and written as each element's literal with them.
  const ints = withBases(array(array(pg("int4"), true, "public.ints"), true, "public._ints"), { "public.ints": "_int4" });
  assert.equal(typed([ints]), false);
  assert.equal(typed([], [{ type: ints }]), true);
  // A structured parameter is written by checks, but its money is checked against the session's locale first.
  const rows = (type: EngineType) => [{ type: array(record([field("amount", type)])), members: [{ written: "amount" }] }];
  assert.equal(typed([], rows(pg("money"))), true);
  assert.equal(typed([], rows(pg("text"))), false);
});
