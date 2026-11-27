// `allow_nested_arrays: true` in the project file: a Postgres array column typed at any depth.
import { test } from "node:test";
import { Ajv2020 } from "ajv/dist/2020.js";
import assert from "node:assert/strict";
import { moduleTypes, reyloImport } from "../src/module-types.ts";
import { querySchemas } from "../src/schema.ts";
import { playResult } from "../src/play-result.ts";
import { analysisWithNestedArrays } from "../src/engine-type.ts";
import type { Analysis } from "../src/engine.ts";

const analysis = {
  params: [],
  columns: [
    { name: "cells", nullable: false, type: { kind: "array", label: "_int4", element: { nullable: true, type: { kind: "integer", bits: 32, label: "int4" } } } },
    { name: "stacked", nullable: false, type: { kind: "array", label: "_int4", element: { type: { kind: "array", label: "_int4", element: { nullable: true, type: { kind: "integer", bits: 32, label: "int4" } } } } } },
  ],
  cardinality: "many",
  kind: "select",
  diagnostics: [],
  rewritten: { sql: "select cells, array_agg(cells) as stacked from grids", spans: [], order: [] },
} as unknown as Analysis;

test("a Postgres array column is typed with one dimension by default, and at any depth under allow_nested_arrays", () => {
  const flat = moduleTypes("", analysis, "postgres", undefined, true);
  assert.match(flat.row, /cells: \(number \| null\)\[\]/);
  assert.match(flat.row, /stacked: \(number \| null\)\[\]\[\]/);
  assert.equal(reyloImport(flat.row), `import type { Query } from "reylo";`);
  const nested = moduleTypes("", analysis, "postgres", undefined, true, ".reylo.yaml", {}, true);
  assert.match(nested.row, /cells: NestedArray<number \| null>/);
  // An array the statement builds of arrays keeps its levels, over the arrays it collects.
  assert.match(nested.row, /stacked: NestedArray<number \| null>\[\]/);
  assert.equal(reyloImport(nested.row), `import type { NestedArray, Query } from "reylo";`);
});

test("an array of a type the format structures as an array, such as bit or lseg, is typed at any depth under allow_nested_arrays", () => {
  const float8 = { kind: "float", exponent_bits: 11, mantissa_bits: 52, label: "float8" };
  const bit = { kind: "array", size: { exact: 3 }, element: { type: { kind: "boolean" } }, label: "bit" };
  const lseg = { kind: "array", size: { exact: 2 }, element: { type: { kind: "struct", fields: [{ name: "x", type: float8 }, { name: "y", type: float8 }], label: "point" } }, label: "lseg" };
  const vector = { kind: "array", element: { type: { kind: "float", exponent_bits: 8, mantissa_bits: 23 } }, label: "public.vector" };
  const arrayOf = (label: string, type: object) => ({ kind: "array", label, element: { nullable: true, type } });
  const columns = [
    { name: "flags", nullable: false, type: arrayOf("_bit", bit) },
    { name: "segments", nullable: false, type: arrayOf("_lseg", lseg) },
    { name: "embeddings", nullable: false, type: arrayOf("public._vector", vector) },
    { name: "doc", nullable: false, type: { kind: "json", label: "jsonb", content: { type: { kind: "struct", fields: [{ name: "flags", type: arrayOf("_bit", bit) }] } } } },
    // `array[flags, flags]`: a level the statement builds over the column's array.
    { name: "stacked", nullable: false, type: { kind: "array", label: "_bit", element: { type: arrayOf("_bit", bit) } } },
  ];
  const whole = { ...analysis, columns } as unknown as Analysis;
  const nested = moduleTypes("", whole, "postgres", undefined, true, ".reylo.yaml", {}, true);
  assert.match(nested.row, /flags: NestedArray<string \| null>;/);
  assert.match(nested.row, /segments: NestedArray<\{ x: number; y: number \}\[\] \| null>;/);
  assert.match(nested.row, /embeddings: NestedArray<number\[\] \| null>;/);
  assert.match(nested.row, /doc: \{ flags: NestedArray<string \| null> \}/);
  assert.match(nested.row, /stacked: NestedArray<string \| null>\[\] \}/);
  const items = (a: Analysis) => (querySchemas(a, "postgres").output as { items: { properties: { segments: { items: unknown } } } }).items.properties.segments.items;
  assert.deepEqual(items(analysisWithNestedArrays(whole)), { anyOf: [items(whole), { type: "array" }] });
});

test("reylo play types an array column as the type check does under allow_nested_arrays", () => {
  assert.match(playResult("", "postgres", { analysis }).element ?? "", /cells: \(number \| null\)\[\]/);
  assert.match(playResult("", "postgres", { analysis }, { allowNestedArrays: true }).element ?? "", /cells: NestedArray<number \| null>/);
});

test("a query's output schema lets an array nest under allow_nested_arrays", () => {
  const items = (a: Analysis) => (querySchemas(a, "postgres").output as { items: { properties: { cells: { items: unknown } } } }).items.properties.cells.items;
  assert.deepEqual(items(analysisWithNestedArrays(analysis)), { anyOf: [items(analysis), { type: "array" }] });
});

test("an array column inside JSON a statement builds is typed at any depth under allow_nested_arrays", () => {
  const built = { ...analysis, columns: [{ name: "doc", nullable: false, type: { kind: "json", label: "jsonb", content: { type: { kind: "struct", fields: [{ name: "cells", type: analysis.columns[0]!.type }] } } } }] } as unknown as Analysis;
  assert.match(moduleTypes("", built, "postgres", undefined, true).row, /cells: \(number \| null\)\[\]/);
  const nested = moduleTypes("", built, "postgres", undefined, true, ".reylo.yaml", {}, true);
  assert.match(nested.row, /cells: NestedArray<number \| null>/);
  assert.equal(reyloImport(nested.row), `import type { NestedArray, Query } from "reylo";`);
});

test("an array of a domain over an array type holds whole arrays: one dimension at each level by default, any depth at both under allow_nested_arrays", () => {
  // `create domain ints as int4[]; create table d (x ints[] not null)`.
  const int4 = { kind: "integer", bits: 32, label: "int4" };
  const ints = { kind: "array", label: "public._ints", element: { nullable: true, type: { kind: "array", label: "public.ints", element: { nullable: true, type: int4 } } } };
  const whole = { ...analysis, columns: [{ name: "x", nullable: false, type: ints }], domains: { "public.ints": "_int4" } } as unknown as Analysis;
  assert.match(moduleTypes("", whole, "postgres", undefined, true).row, /x: \(\(number \| null\)\[\] \| null\)\[\]/);
  assert.match(moduleTypes("", whole, "postgres", undefined, true, ".reylo.yaml", {}, true).row, /x: NestedArray<NestedArray<number \| null> \| null>/);
  // A parameter writes each whole array as one element, so the level outside them is one dimension.
  const param = { ...whole, params: [{ name: "x", nullable: false, type: ints }] } as unknown as Analysis;
  assert.match(moduleTypes("", param, "postgres", undefined, true, ".reylo.yaml", {}, true).params, /x: NestedArray<number>\[\]/);
});

test("a Postgres array parameter is typed with one dimension by default, and at any depth under allow_nested_arrays, so it takes back what a column of its type returns", () => {
  const int4 = { kind: "integer", bits: 32, label: "int4" };
  const float8 = { kind: "float", exponent_bits: 11, mantissa_bits: 52, label: "float8" };
  const lseg = { kind: "array", size: { exact: 2 }, element: { type: { kind: "struct", fields: [{ name: "x", type: float8 }, { name: "y", type: float8 }], label: "point" } }, label: "lseg" };
  const withParams = {
    ...analysis,
    params: [
      { name: "cells", nullable: false, type: { kind: "array", label: "_int4", element: { type: int4 } } },
      { name: "segments", nullable: false, type: { kind: "array", label: "_lseg", element: { type: lseg } } },
    ],
  } as unknown as Analysis;
  const flat = moduleTypes("", withParams, "postgres", undefined, true);
  assert.match(flat.params, /cells: number\[\]; segments: \{ x: number; y: number \}\[\]\[\]/);
  const nested = moduleTypes("", withParams, "postgres", undefined, true, ".reylo.yaml", {}, true);
  assert.match(nested.params, /cells: NestedArray<number>; segments: NestedArray<\{ x: number; y: number \}\[\]>/);
  assert.equal(reyloImport(`${nested.params} ${nested.row}`), `import type { NestedArray, Query } from "reylo";`);
  // The input's JSON Schema lets them nest too, naming the element at each level, to Postgres's six dimensions.
  const validate = new Ajv2020({ strict: false }).compile(querySchemas(analysisWithNestedArrays(withParams), "postgres").input);
  const seg = [{ x: 0, y: 1 }, { x: 2, y: 3 }];
  for (const cells of [[1], [[1, 2]], [[[[[[1]]]]]]]) assert.equal(validate({ cells, segments: [[seg]] }), true, JSON.stringify(cells));
  for (const cells of [[["x"]], [[null]], [[[[[[[1]]]]]]]]) assert.equal(validate({ cells, segments: [seg] }), false, JSON.stringify(cells));
  assert.equal(validate({ cells: [1], segments: [[[{ x: 0 }]]] }), false);
  assert.equal(validate({ cells: [[1]], segments: [] }), true);
  // Each nested array's element is in $defs once, so an array in a row in an array adds to the schema rather than multiplying it.
  let row: object = { kind: "integer", bits: 32, label: "int4" };
  for (let level = 0; level < 6; level++) row = { kind: "struct", label: `public.r${level}`, fields: [{ name: "f", type: { kind: "array", label: `public._r${level}`, element: { type: row } } }] };
  const wide = { ...analysis, params: [{ name: "r", nullable: false, type: row }] } as unknown as Analysis;
  assert.ok(JSON.stringify(querySchemas(analysisWithNestedArrays(wide), "postgres").input).length < 20_000);
  // A JSON element may be an array itself, which a parameter writes whole, so a jsonb[] stays one level.
  const docs = { ...analysis, params: [{ name: "docs", nullable: false, type: { kind: "array", label: "_jsonb", element: { type: { kind: "json", label: "jsonb" } } } }] } as unknown as Analysis;
  assert.match(moduleTypes("", docs, "postgres", undefined, true, ".reylo.yaml", {}, true).params, /docs: JsonInput\[\]/);
});
