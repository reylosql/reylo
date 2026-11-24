import { posts } from '@/lib/blog';
import { blogIndexMarkdown, blogMarkdownOf } from '@/lib/markdown';

// The blog index at /blog.md and each post at /blog/<slug>.md, written to build/client at build time.
// vercel.json serves these at /blog and /blog/<slug> when a client sends `Accept: text/markdown`.
export async function loader({ request }: { request: Request }) {
  const pathname = new URL(request.url).pathname.replace(/\.data$/, '');
  if (pathname === '/blog.md') {
    return new Response(blogIndexMarkdown(posts), {
      headers: { 'Content-Type': 'text/markdown; charset=utf-8' },
    });
  }
  const slug = pathname.replace(/^\/blog\/|\.md$/g, '');
  const post = posts.find((p) => p.slug === slug);
  if (!post) throw new Response('Not found', { status: 404 });
  return new Response(await blogMarkdownOf(slug, post), {
    headers: { 'Content-Type': 'text/markdown; charset=utf-8' },
  });
}
