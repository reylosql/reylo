import { posts } from '@/lib/blog';
import { apiMarkdown, REYLO_API } from '@/lib/live/api-docs';
import { markdownPath, pageGroups, siteUrl } from '@/lib/layout.shared';
import { markdownOf } from '@/lib/markdown';

// Every page as Markdown, for a coding agent or a language model (https://llmstxt.org): a concise
// page index at the top (so an agent can fetch a single page's .md file directly), followed by the
// full Markdown of every page in sidebar order, plus unlisted guides (Prisma, Type Defs) and the
// playground's API (/play/api.md). Written to build/client/llms.txt at build time.

/** Map a sidebar href back to its content/docs slug ('' for index.mdx). */
function slugOf(href: string): string {
  if (href === '/docs') return '';
  if (href.startsWith('/docs/migrate/')) return `migrate-${href.slice('/docs/migrate/'.length)}`;
  return href.replace(/^\/docs\//, '');
}

const EXTRA_GUIDES: readonly { title: string; slug: string }[] = [
  { title: 'Migrating from Prisma', slug: 'prisma' },
  { title: 'Reylo Type Defs', slug: 'typedefs' },
];

const PAGES: readonly string[] = [
  ...pageGroups.flatMap((group) => group.items.map((item) => slugOf(item.href))),
  ...EXTRA_GUIDES.map((g) => g.slug),
];

/** First blockquote line of a rendered page Markdown (its frontmatter description). */
function summaryOf(md: string): string {
  const m = /^> (.+)$/m.exec(md);
  return m ? m[1]!.replace(/^"(.*)"$/, '$1') : '';
}

export async function loader() {
  const { playData } = await import('@/lib/live/play-data');
  const rendered = await Promise.all(PAGES.map(async (slug) => [slug, await markdownOf(slug)] as const));
  const bySlug = new Map(rendered);
  const api = apiMarkdown(REYLO_API, (await playData()).apiAnswer);

  const indexLines: string[] = [
    '# Reylo',
    '',
    '> Reylo is a type inference engine for SQL and a SQL-first schema migration tool for Postgres, MySQL, SQLite and ClickHouse. Each page below is also available individually as Markdown (send `Accept: text/markdown` to any page URL, or fetch its `.md` URL directly).',
    '',
  ];
  for (const group of pageGroups) {
    indexLines.push(`## ${group.label}`, '');
    for (const item of group.items) {
      const slug = slugOf(item.href);
      const summary = summaryOf(bySlug.get(slug) ?? '');
      indexLines.push(`- [${item.title}](${siteUrl}${markdownPath(slug)})${summary ? `: ${summary}` : ''}`);
    }
    indexLines.push('');
  }
  indexLines.push('## Guides and tools', '');
  for (const guide of EXTRA_GUIDES) {
    const summary = summaryOf(bySlug.get(guide.slug) ?? '');
    indexLines.push(`- [${guide.title}](${siteUrl}${markdownPath(guide.slug)})${summary ? `: ${summary}` : ''}`);
  }
  indexLines.push(`- [Playground API](${siteUrl}/play/api.md): Analyze a Postgres query over HTTP without a local database.`, '');
  indexLines.push('## Blog', '');
  for (const post of posts) {
    indexLines.push(`- [${post.title}](${siteUrl}/blog/${post.slug}.md) (${post.date}): ${post.description}`);
  }
  indexLines.push('', '---', '');

  const text = [indexLines.join('\n'), ...rendered.map(([, md]) => md), api].join('\n');
  return new Response(text, { headers: { 'Content-Type': 'text/markdown; charset=utf-8' } });
}
