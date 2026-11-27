// A post's link preview, at /og/blog/<name>.png (app/routes.ts): its title on the card Docs Kit draws
// for each docs page (docs.config.ts).
import { ogCard } from 'docs-kit/og';
import site from '@/docs.config';
import { blogUrl, posts } from '@/lib/blog';
import { siteUrl } from '@/lib/layout.shared';

export async function loader({ request }: { request: Request }) {
  const name = new URL(request.url).pathname.replace(/^\/og\/blog\/|\.png(?:\.data)?$/g, '');
  const post = posts.find((p) => p.slug === name);
  if (!post) throw new Response('Not found', { status: 404 });
  return ogCard({ slug: `blog/${post.slug}`, title: post.title, url: `${siteUrl}${blogUrl}/${post.slug}`, name: site.name }, site.og);
}
