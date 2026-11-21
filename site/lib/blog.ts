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
  }))
  .sort((a, b) => b.date.localeCompare(a.date) || b.slug.localeCompare(a.slug));
