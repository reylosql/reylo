import type { ShikiTransformer } from 'shiki';

/** A hast node, as much of one as this file reads and writes. */
type HastNode = { type: string; tagName?: string; value?: string; properties?: Record<string, unknown>; children?: HastNode[] };

/*
 * A table's declaration on hover, in every `sql` block of a page: each name of a table in the
 * page's schema, outside strings and comments and not after a dot, is drawn as a Twoslash hover
 * whose popup holds that table's `create table`, as the homepage's examples show it
 * (components/home-landing.tsx). lib/remark-mapper.ts knows each block's dialect and the page's
 * `schema.sql` blocks, so it registers each block's tables here and names them in the block's meta,
 * `tables="<key>"`; this transformer reads them back. The popup is the hast Fumadocs' Twoslash
 * renderer builds (fumadocs-twoslash), so the docs' Twoslash components draw it.
 */

/** Each block's tables, by the key its meta names: a table's name, lowercase, to its declaration. */
const registry = new Map<string, Record<string, string>>();

/** Registers a block's tables and returns the meta attribute that names them. */
export function tablesMeta(key: string, tables: Map<string, string>): string {
  registry.set(key, Object.fromEntries(tables));
  return `tables="${key}"`;
}

/** Each `create table` of a schema, by the table's name, with the comment lines above it. */
export function declarations(schema: string): Map<string, string> {
  const out = new Map<string, string>();
  for (const statement of statements(schema)) {
    const name = /^(?:\s*--[^\n]*\n)*\s*create\s+(?:temporary\s+)?table\s+(?:if\s+not\s+exists\s+)?[`"]?(\w+)/i.exec(statement)?.[1];
    if (name) out.set(name.toLowerCase(), `${statement.trim()};`);
  }
  return out;
}

/** A script's statements, split at each semicolon outside a string. */
function statements(script: string): string[] {
  const out: string[] = [];
  let start = 0;
  for (let i = 0; i < script.length; i++) {
    const c = script[i];
    if (c === "'") i = script.indexOf("'", i + 1) < 0 ? script.length : script.indexOf("'", i + 1);
    else if (c === '-' && script[i + 1] === '-') i = script.indexOf('\n', i) < 0 ? script.length : script.indexOf('\n', i);
    else if (c === ';') {
      out.push(script.slice(start, i));
      start = i + 1;
    }
  }
  if (script.slice(start).trim()) out.push(script.slice(start));
  return out;
}

/** Where a statement names one of the tables, as [start, end, table]: an identifier outside strings and comments, not after a dot or `as`, and not called as a function. */
export function tableNames(sql: string, tables: Record<string, string>): [number, number, string][] {
  const out: [number, number, string][] = [];
  for (let i = 0; i < sql.length; ) {
    const c = sql[i]!;
    if (c === "'") i = sql.indexOf("'", i + 1) < 0 ? sql.length : sql.indexOf("'", i + 1) + 1;
    else if (c === '-' && sql[i + 1] === '-') i = sql.indexOf('\n', i) < 0 ? sql.length : sql.indexOf('\n', i);
    else if (/[A-Za-z_]/.test(c)) {
      let end = i;
      while (end < sql.length && /\w/.test(sql[end]!)) end++;
      const word = sql.slice(i, end).toLowerCase();
      if (word in tables && !/(?:\.|\bas)\s*$/i.test(sql.slice(0, i)) && sql[i - 1] !== ':' && sql[end] !== '(') out.push([i, end, word]);
      i = end;
    } else i++;
  }
  return out;
}

const MARK = 'data-reylo-table';

export const tableHovers: ShikiTransformer = {
  name: 'reylo:table-hovers',
  preprocess(code, options) {
    const raw = (options.meta as { __raw?: string } | undefined)?.__raw ?? '';
    const key = /(?:^|\s)tables="([^"]+)"/.exec(raw)?.[1];
    const tables = key ? registry.get(key) : undefined;
    if (!tables || options.lang !== 'sql') return;
    const spans = tableNames(code, tables);
    if (!spans.length) return;
    (this.meta as { reyloTables?: Record<string, string> }).reyloTables = tables;
    // Shiki's decorations split the tokens at each name and wrap it; `root` below turns each wrapper into a hover.
    options.decorations = [...(options.decorations ?? []), ...spans.map(([start, end, table]) => ({ start, end, properties: { [MARK]: table } }))];
  },
  // docs-kit draws a hover's dotted rule inside a `.twoslash` block, as a `ts twoslash` block is.
  pre(pre) {
    if ((this.meta as { reyloTables?: unknown }).reyloTables) this.addClassToHast(pre, 'twoslash');
  },
  root(root) {
    const tables = (this.meta as { reyloTables?: Record<string, string> }).reyloTables;
    if (!tables) return;
    const popups = new Map<string, HastNode[]>();
    const popup = (table: string) => {
      if (!popups.has(table)) {
        const hast = this.codeToHast(tables[table]!, { ...this.options, lang: 'sql', meta: {}, transformers: [], decorations: [] });
        const code = (hast as unknown as HastNode).children![0]!.children![0]!.children!;
        popups.set(table, code);
      }
      return popups.get(table)!;
    };
    const visit = (node: HastNode) => {
      node.children?.forEach((child, i) => {
        if (child.type !== 'element') return;
        const table = child.properties?.[MARK];
        if (typeof table !== 'string') return visit(child);
        delete child.properties![MARK];
        node.children![i] = {
          type: 'element',
          tagName: 'Popup',
          properties: { class: 'twoslash-hover' },
          children: [
            {
              type: 'element',
              tagName: 'PopupContent',
              properties: { class: 'twoslash-popup-container nd-copy-ignore' },
              children: [{ type: 'element', tagName: 'div', properties: { class: 'twoslash shiki fd-codeblock prose-no-margin reylo-table-decl' }, children: [{ type: 'element', tagName: 'code', properties: { class: 'twoslash-popup-code' }, children: popup(table) }] }],
            },
            { type: 'element', tagName: 'PopupTrigger', properties: {}, children: [child] },
          ],
        };
      });
    };
    visit(root as unknown as HastNode);
  },
};
