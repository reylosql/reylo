import { readFileSync } from 'node:fs';
import path from 'node:path';
import { use, useMemo } from 'react';
import { DocsShell } from 'docs-kit/shell';
import type { Root } from 'fumadocs-core/page-tree';
import type { Route } from './+types/blog-post';
import { Brand } from '@/components/brand';
import { getMDXComponents } from '@/components/mdx';
import { PlayDataContext } from '@/components/play-embed';
import { blogUrl, posts } from '@/lib/blog';
import { loadPost } from '@/lib/blog-content';
import { blogMeta } from '@/lib/blog-meta';
import { ogHash } from '@/lib/blog-og.server';
import { docsTitle, githubUrl, pageGroups, pageLinks } from '@/lib/layout.shared';
import { source } from '@/lib/source';

/** Run at build time, when each post is rendered to a file (react-router.config.ts): the post, without its content, which the component loads itself (lib/blog-content.ts). */
export async function loader({ params }: Route.LoaderArgs) {
  const post = posts.find((p) => p.slug === params.slug);
  if (!post) throw new Response('Not found', { status: 404 });
  // The playground's data, for a post that embeds it (components/play-embed.tsx).
  const embeds = readFileSync(path.join('content', 'blog', post.path), 'utf8').includes('<PlayEmbed');
  const play = embeds ? await (await import('@/lib/live/play-data')).playData() : undefined;
  return { post, tree: source.getPageTree(), og: await ogHash(), play };
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

/** A post: docs-kit's page, with the docs' page list on the left and its table of contents on the right, its date and a release's GitHub release under the title. */
function PostContent({ loaderData }: Pick<Route.ComponentProps, 'loaderData'>) {
  const { post, tree } = loaderData;
  const file = use(loadPost(post.path));
  const toc = file.toc.filter((item) => item.depth <= 3);
  return (
    <DocsShell
      tree={tree as Root}
      title={post.title}
      toc={toc.length < 2 ? [] : toc}
      pages={{ groups: pageGroups, links: pageLinks, linksLabel: 'Links', current: blogUrl }}
      pageTitle="Blog"
      brand={<Brand />}
      github={githubUrl}
      star
      footer={<>Released under the MIT License. Copyright © 2026 Colin McDonnell.</>}
    >
      <p className="dk-subheader">
        <time dateTime={post.date}>{post.shown}</time>
        {post.release && (
          <>
            {' · '}
            <a href={post.release.url}>{post.release.tag} on GitHub</a>
          </>
        )}
      </p>
      <PlayDataContext value={loaderData.play}>
        <file.default components={getMDXComponents()} />
      </PlayDataContext>
    </DocsShell>
  );
}
