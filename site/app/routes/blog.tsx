import type { Route } from './+types/blog';
import { BlogIndex } from '@/components/blog-index';
import { blogUrl, posts } from '@/lib/blog';
import { blogMeta } from '@/lib/blog-meta';
import { ogHash } from '@/lib/blog-og.server';
import { docsTitle } from '@/lib/layout.shared';
import { fetchStars } from '@/lib/stars';

const description = 'What each release of Reylo changed.';

/** Run at build time, when the page is rendered to a file (react-router.config.ts): the posts, newest first, and the star count for the bar's GitHub pill. */
export async function loader() {
  return { posts, og: await ogHash(), stars: await fetchStars() };
}

export function meta({ loaderData }: Route.MetaArgs): Route.MetaDescriptors {
  if (!loaderData) return [];
  return blogMeta({ title: docsTitle('Blog'), description, path: blogUrl, og: loaderData.og, type: 'website' });
}

/** The blog, at /blog: the newest announcement as a card, then the releases (components/blog-index.tsx). */
export default function Blog({ loaderData }: Route.ComponentProps) {
  return <BlogIndex posts={loaderData.posts} stars={loaderData.stars} />;
}
