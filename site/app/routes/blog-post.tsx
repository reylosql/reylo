import { readFileSync } from 'node:fs';
import path from 'node:path';
import { use, useMemo } from 'react';
import { DocsBody, DocsTitle } from 'fumadocs-ui/layouts/docs/page';
import type { Route } from './+types/blog-post';
import { Foot, Nav } from '@/components/home-chrome';
import { getMDXComponents } from '@/components/mdx';
import { PlayDataContext } from '@/components/play-embed';
import { blogUrl, posts } from '@/lib/blog';
import { loadPost } from '@/lib/blog-content';
import { blogMeta } from '@/lib/blog-meta';
import { ogHash } from '@/lib/blog-og.server';
import { docsTitle } from '@/lib/layout.shared';
import { fetchStars } from '@/lib/stars';
import '@/components/blog-post.css';

/** Run at build time, when each post is rendered to a file (react-router.config.ts): the post, without its content, which the component loads itself (lib/blog-content.ts). */
export async function loader({ params }: Route.LoaderArgs) {
  const post = posts.find((p) => p.slug === params.slug);
  if (!post) throw new Response('Not found', { status: 404 });
  // The playground's data, for a post that embeds it (components/play-embed.tsx).
  const embeds = readFileSync(path.join('content', 'blog', post.path), 'utf8').includes('<PlayEmbed');
  const play = embeds ? await (await import('@/lib/live/play-data')).playData() : undefined;
  return { post, og: await ogHash(), stars: await fetchStars(), play };
}

export function meta({ loaderData }: Route.MetaArgs): Route.MetaDescriptors {
  if (!loaderData) return [];
  const { post, og } = loaderData;
  return blogMeta({ title: docsTitle(post.title), description: post.description, path: `${blogUrl}/${post.slug}`, og, type: 'article' });
}

/** A post is the same tree on every render of its route, as a docs page is (app/routes/page.tsx), so a jump to one of its headings is the browser's own. */
export default function BlogPost({ loaderData }: Route.ComponentProps) {
  return useMemo(() => <PostContent loaderData={loaderData} />, [loaderData]);
}

/**
 * A post: under the homepage's bar (components/home-chrome.tsx), as the blog's index is, one column
 * in the middle of the window with no page list and no table of contents, its byline under the title:
 * the author's avatar and name where the post names one, its date, and a release's GitHub release. The column is the docs' own page element, `#nd-page` with
 * Fumadocs' title and body, so the docs' components look in a post as they do in the docs; a
 * playground (components/play-embed.tsx) is wider than the column (components/blog-post.css).
 */
function PostContent({ loaderData }: Pick<Route.ComponentProps, 'loaderData'>) {
  const { post } = loaderData;
  const file = use(loadPost(post.path));
  return (
    <div className="ml rh bl bp">
      <Nav stars={loaderData.stars} />
      <main className="bp-main">
        <article id="nd-page" className="bp-article">
          <DocsTitle>{post.title}</DocsTitle>
          <div className="bp-byline">
            {post.author && (
              <a className="bp-author" href={post.author.url}>
                <img src={post.author.avatar} alt="" width={36} height={36} />
              </a>
            )}
            <div className="bp-byline-text">
              {post.author && (
                <a className="bp-author-name" href={post.author.url}>
                  {post.author.name}
                </a>
              )}
              <span className="bp-byline-meta">
                <time dateTime={post.date}>{post.shown}</time>
                {post.release && (
                  <>
                    {' · '}
                    <a href={post.release.url}>{post.release.tag} on GitHub</a>
                  </>
                )}
              </span>
            </div>
          </div>
          <DocsBody>
            <PlayDataContext value={loaderData.play}>
              <file.default components={getMDXComponents()} />
            </PlayDataContext>
          </DocsBody>
        </article>
      </main>
      <Foot />
    </div>
  );
}
