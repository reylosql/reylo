import { readFileSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { pageMarkdown } from 'docs-kit/remark-sections';
import { expandCompare } from '@/lib/compare';
import { docsUrl, siteUrl } from '@/lib/layout.shared';
import { expandInferenceCards } from '@/lib/inference-card-markdown';
import { PAGE_SECTIONS } from '@/lib/page-sections';
import { rustContent } from '@/lib/rust-content';
import { expandErrors } from '@/lib/sql-errors';
import { DATABASE_NAMES, DATABASES } from '@/lib/typedefs';

/** Each `<Step>` as a numbered list item, its name in bold and where it acts after it. */
function expandSteps(text: string): string {
  return text.replace(/<Steps>\n([\s\S]*?)<\/Steps>\n/g, (_, steps: string) => {
    let n = 0;
    return steps.replace(/<Step title="([^"]+)"(?: where="([^"]+)")?>\n+([\s\S]*?)\n+<\/Step>\n/g, (_m, title: string, where: string | undefined, body: string) => `${++n}. **${title}**${where ? ` (${where})` : ''}: ${body}\n`);
  });
}

/** A heading's right-aligned code (`<code className="dk-aside">`) as plain inline code. */
const expandAsides = (text: string): string => text.replace(/<code className="dk-aside">([^<]+)<\/code>/g, '`$1`');

/** An inline HTML link (such as a callout's CTA button) as a Markdown link, dropping decorative aria-hidden spans. */
const expandAnchors = (text: string): string =>
  text.replace(/<a\b[^>]*href="([^"]+)"[^>]*>([\s\S]*?)<\/a>/g, (_, href: string, inner: string) => {
    const label = inner.replace(/<span\b[^>]*aria-hidden="true"[^>]*>[\s\S]*?<\/span>/g, '').trim();
    return `[${label}](${href})`;
  });

/** Each `<TypeDef>` card with its `holds` values and per-database types, and `<TypeDefsSpec />` as the TypeScript spec. */
function expandTypeDefs(text: string): string {
  if (!text.includes('<TypeDef')) return text;
  let out = text.replace(/<TypeDef([^\n]*)>\n+([\s\S]*?)\n+<\/TypeDef>/g, (_, attrs: string, body: string) => {
    const holdsMatch = /\bholds=\{(\[[\s\S]*?\])\}/.exec(attrs);
    const typesMatch = /\btypes=\{(\{[\s\S]*?\})\}/.exec(attrs);
    const parts = [body.trim()];
    if (holdsMatch) {
      const holds = JSON.parse(holdsMatch[1]!) as string[];
      if (holds.length) parts.push(`Holds: ${holds.map((v) => `\`${v}\``).join(', ')}`);
    }
    if (typesMatch) {
      const types = JSON.parse(typesMatch[1]!) as Partial<Record<(typeof DATABASES)[number], string[]>>;
      const lines = DATABASES.filter((db) => types[db]?.length).map((db) => `- **${DATABASE_NAMES[db]}:** ${types[db]!.map((t) => (/^(?:an?|every|any) /.test(t) ? t : `\`${t}\``)).join(', ')}`);
      if (lines.length) parts.push(`Databases:\n\n${lines.join('\n')}`);
    }
    return parts.join('\n\n');
  });
  if (out.includes('<TypeDefsSpec />')) {
    const specDir = path.resolve('..', 'spec');
    const schemaId = String(JSON.parse(readFileSync(path.join(specDir, 'typedefs.schema.json'), 'utf8')).$id);
    const spec = readFileSync(path.join(specDir, 'index.ts'), 'utf8')
      .replace(/^\/\/.*\n/gm, '')
      .replace(/^[ \t]*\/\*\*[\s\S]*?\*\/\n/gm, '')
      .replace(/\n{3,}/g, '\n\n')
      .trim();
    out = out.replace('<TypeDefsSpec />', `JSON Schema: ${schemaId}\n\nEvery type and field, as TypeScript, generated from the format's JSON Schema:\n\n\`\`\`ts\n${spec}\n\`\`\``);
  }
  return out;
}

/**
 * A page of content/docs as Markdown, by its slug ('' for the main page): its files in order (a
 * page's sections are lib/page-sections.ts'), read as each heading's copy button reads its section, under
 * its title and description, with each link made absolute (docs-kit's pageMarkdown). Each page's
 * is written to build/client/<slug>.md (index.md for the main page) by app/routes/markdown.ts, which
 * vercel.json serves at the page's own address to an agent that asks for Markdown, and
 * /llms.txt is all of them (app/routes/llms.ts). Each file is read as the site builds it, its Rust
 * content left out while the docs leave Rust out (lib/rust-content.ts).
 */
export async function markdownOf(slug: string): Promise<string> {
  const own = `${slug || 'index'}.mdx`;
  const files = [path.resolve('content/docs', own), ...(PAGE_SECTIONS[own] ?? []).map((name) => path.resolve('content/sections', name))];
  // Each inference card with the types the page shows for it (lib/inference-card-markdown.ts), and each
  // error example with its errors (lib/sql-errors.ts), and the comparison's marks as a table (lib/compare.ts).
  const sources = (await Promise.all(files.map((file) => readFile(file, 'utf8')))).map((text) => expandTypeDefs(expandAnchors(expandAsides(expandSteps(expandCompare(expandErrors(expandInferenceCards(rustContent(text)))))))));
  return pageMarkdown(sources, { site: siteUrl, page: docsUrl(slug ? `/${slug}` : '/') });
}
