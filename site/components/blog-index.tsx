import type { ReactNode } from 'react';
import { Link } from 'react-router';
import { Foot, Nav } from './home-chrome';
import type { Post } from '@/lib/blog';
import './blog-index.css';

/*
 * The blog's index, at /blog (app/routes/blog.tsx), under the homepage's bar: the newest post that
 * isn't a release, as a card beside a glimpse of what it announces, then each release as a row led
 * by its version. Every post is the docs' own content/blog.
 */

/** What each announcement's card shows beside its words, by the post's name. A post with none gets the words alone. */
const GLIMPSES: Record<string, () => ReactNode> = {
  'reylo-migrate': MigrateGlimpse,
};

/** The blog's address; lib/blog.ts holds it too, but reads the posts with Node's APIs, so the browser can't import it. */
const blogUrl = '/blog';

export function BlogIndex({ posts, stars }: { posts: Post[]; stars?: number }) {
  const featured = posts.find((p) => !p.release);
  const releases = posts.filter((p) => p.release);
  const Glimpse = featured ? GLIMPSES[featured.slug] : undefined;
  return (
    <div className="ml rh bl">
      <Nav stars={stars} />
      <main className="bl-main">
        <h1 className="bl-h1">Blog</h1>
        {featured ? (
          <Link className={Glimpse ? 'bl-feat' : 'bl-feat bl-feat-words-only'} to={`${blogUrl}/${featured.slug}`}>
            <div className="bl-feat-words">
              <span className="bl-new">New</span>
              <h2>{featured.title}</h2>
              <p>{featured.description}</p>
              <span className="bl-read">
                Read the announcement <span aria-hidden="true">→</span>
              </span>
            </div>
            {Glimpse ? <Glimpse /> : null}
          </Link>
        ) : null}
        <div className="bl-section">
          <span>Releases</span>
        </div>
        <ul className="bl-releases">
          {releases.map((post) => (
            <li key={post.slug}>
              <Link to={`${blogUrl}/${post.slug}`}>
                <span className="bl-version">{post.release!.tag}</span>
                <span className="bl-description">{post.description}</span>
                <time dateTime={post.date}>{post.shown.replace(/, \d{4}$/, '')}</time>
              </Link>
            </li>
          ))}
        </ul>
      </main>
      <Foot />
    </div>
  );
}

/** Reylo Migrate's card: an edit to schema.sql, as the Migrate landing's diff-engine row makes it. */
function MigrateGlimpse() {
  const kw = (s: string) => <span className="bl-kw">{s}</span>;
  const ty = (s: string) => <span className="bl-ty">{s}</span>;
  return (
    <div className="bl-win" aria-hidden="true">
      <div className="bl-bar">reylo/schema.sql</div>
      <pre>
        <span className="bl-l">
          {kw('create table')} posts (
        </span>
        <span className="bl-l">
          {'  id '}
          {ty('bigint')} {kw('primary key')},
        </span>
        <span className="bl-l bl-del" data-g="-">
          {'  title '}
          {ty('text')},
        </span>
        <span className="bl-l bl-add" data-g="+">
          {'  title '}
          {ty('text')} {kw('not null check')} (length(title) {'<='} 200),
        </span>
        <span className="bl-l bl-add" data-g="+">
          {'  published '}
          {ty('boolean')} {kw('not null default')} false,
        </span>
        <span className="bl-l">
          {'  body '}
          {ty('text')} {kw('not null')}
        </span>
        <span className="bl-l">);</span>
      </pre>
    </div>
  );
}
