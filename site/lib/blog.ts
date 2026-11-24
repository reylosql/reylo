import { blog } from 'collections/server';
import { githubUrl } from '@/lib/layout.shared';

/** The blog's address. */
export const blogUrl = '/blog';

/** A post as its page and the blog's list show it. */
export interface Post {
  /** Its address under /blog, its file's name. */
  slug: string;
  /** Its file in content/blog. */
  path: string;
  title: string;
  description: string;
  /** Its date as the frontmatter writes it, YYYY-MM-DD. */
  date: string;
  /** Its date as a reader reads it: November 21, 2026. */
  shown: string;
  /** A release's post: its GitHub release, which lists every change. */
  release?: { tag: string; url: string };
  /** Who wrote it, in its byline: their name, their avatar and the page their name links. */
  author?: Author;
}

export interface Author {
  name: string;
  avatar: string;
  url: string;
}

/** The authors a post's frontmatter may name, by name: `author: Colin McDonnell`. */
const AUTHORS: Record<string, Author> = {
  'Colin McDonnell': { name: 'Colin McDonnell', avatar: '/blog/colinhacks.png', url: 'https://github.com/colinhacks' },
};

function author(name: string | undefined, path: string): Author | undefined {
  if (name === undefined) return undefined;
  const found = AUTHORS[name];
  if (!found) throw new Error(`content/blog/${path}: no author named "${name}" in lib/blog.ts`);
  return found;
}

// The posts of content/blog, newest first. Read at build time only, by the routes' loaders; the
// browser loads each post's content itself (lib/blog-content.ts).
export const posts: Post[] = blog
  .map((entry) => ({
    slug: entry.info.path.replace(/\.mdx?$/, ''),
    path: entry.info.path,
    title: entry.title,
    description: entry.description,
    date: entry.date,
    shown: new Date(`${entry.date}T00:00:00Z`).toLocaleDateString('en-US', { year: 'numeric', month: 'long', day: 'numeric', timeZone: 'UTC' }),
    release: entry.release ? { tag: entry.release, url: `${githubUrl}/releases/tag/${entry.release}` } : undefined,
    author: author(entry.author, entry.info.path),
  }))
  .sort((a, b) => b.date.localeCompare(a.date) || b.slug.localeCompare(a.slug));
