// A docs site's pages as files, read from its folder: what a page is made of, for the routes that
// serve its Markdown and for a site's build-time scripts and remark plugins, which load this in Node
// itself (so it imports nothing that only Vite resolves).
import { existsSync, readdirSync } from 'node:fs';
import path from 'node:path';

/** Every page of `content/docs` by slug, `''` for `index.mdx`: the first page first, the rest by name. */
export function pageSlugs(root = process.cwd()): string[] {
  const docs = path.join(root, 'content', 'docs');
  const names = existsSync(docs) ? readdirSync(docs).filter((name) => name.endsWith('.mdx')).map((name) => name.slice(0, -'.mdx'.length)).sort() : [];
  return [...(names.includes('index') ? [''] : []), ...names.filter((name) => name !== 'index')];
}

/**
 * The first page's files, in the order the page shows them: `content/docs/index.mdx`, then each file
 * of `content/sections/`, one per h2 section, by name, so that an edit compiles one section.
 */
export function pageFiles(root = process.cwd()): string[] {
  const sections = path.join(root, 'content', 'sections');
  const names = existsSync(sections) ? readdirSync(sections).filter((name) => name.endsWith('.mdx')).sort() : [];
  return [path.join(root, 'content', 'docs', 'index.mdx'), ...names.map((name) => path.join(sections, name))];
}

/**
 * Whether Docs Kit draws a page's link preview (`docs-kit/og`): every page without its own image,
 * `public/og/<slug>.png` (`index.png` for the first), but the first page when the site draws its own
 * card, `app/routes/opengraph-image.tsx`, which stays the first page's.
 */
export function drawsCard(slug: string, root = process.cwd()): boolean {
  if (existsSync(path.join(root, 'public', 'og', `${slug || 'index'}.png`))) return false;
  return slug !== '' || !existsSync(path.join(root, 'app', 'routes', 'opengraph-image.tsx'));
}

/** The pages whose link preview Docs Kit draws (`drawsCard`), of `pages`: every page of `content/docs` by default. */
export function ogSlugs(pages: readonly string[] = pageSlugs(), root = process.cwd()): string[] {
  return pages.filter((slug) => drawsCard(slug, root));
}

/** Each page's files, the first page first: its files (`pageFiles`), then every other page's one file, by name. */
export function sitePages(root = process.cwd()): string[][] {
  return pageSlugs(root).map((slug) => (slug ? [path.join(root, 'content', 'docs', `${slug}.mdx`)] : pageFiles(root)));
}
