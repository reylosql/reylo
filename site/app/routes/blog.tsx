import { DocsShell } from 'docs-kit/shell';
import type { Root } from 'fumadocs-core/page-tree';
import { Link } from 'react-router';
import type { Route } from './+types/blog';
import { Brand } from '@/components/brand';
import { blogUrl, posts } from '@/lib/blog';
import { blogMeta, ogHash } from '@/lib/blog-meta';
import { docsTitle, githubUrl, pageGroups, pageLinks } from '@/lib/layout.shared';
import { source } from '@/lib/source';

const description = 'What each release of Reylo changed.';

/** Run at build time, when the page is rendered to a file (react-router.config.ts): the posts, newest first. */
export async function loader() {
  return { posts, tree: source.getPageTree(), og: await ogHash() };
}

export function meta({ loaderData }: Route.MetaArgs): Route.MetaDescriptors {
  if (!loaderData) return [];
  return blogMeta({ title: docsTitle('Blog'), description, path: blogUrl, og: loaderData.og, type: 'website' });
}

/** The blog, at /blog: docs-kit's page, with the docs' page list on the left, and each post's title, date and description. */
export default function Blog({ loaderData }: Route.ComponentProps) {
  const { tree } = loaderData;
  return (
    <DocsShell
      tree={tree as Root}
      title="Blog"
      toc={[]}
      pages={{ groups: pageGroups, links: pageLinks, linksLabel: 'Links', current: blogUrl }}
      pageTitle="Blog"
      brand={<Brand />}
      github={githubUrl}
      footer={<>Released under the MIT License. Copyright © 2026 Colin McDonnell.</>}
    >
      <p className="dk-subheader">{description}</p>
      <ul className="blog-list not-prose">
        {loaderData.posts.map((post) => (
          <li key={post.slug}>
            <Link to={`${blogUrl}/${post.slug}`}>
              <span className="blog-list-title">{post.title}</span>
              <time dateTime={post.date}>{post.shown}</time>
            </Link>
            <p>{post.description}</p>
          </li>
        ))}
      </ul>
    </DocsShell>
  );
}
