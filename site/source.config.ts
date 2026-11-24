import { defineCollections, defineConfig, defineDocs, frontmatterSchema } from 'fumadocs-mdx/config';

// The pages are content/docs. Usage and Inference go on in content/sections/ (lib/page-sections.ts), so that an edit
// compiles one section. Both take lib/mdx-options.ts, imported inside the options so that only the
// build loads it (and TypeScript with it). The Rust page is built too, unlisted while the docs leave
// Rust out (lib/rust-content.ts).
const mdxOptions = async (environment: Parameters<typeof import('./lib/mdx-options.ts').mdxOptions>[0]) => (await import('./lib/mdx-options.ts')).mdxOptions(environment);

export const docs = defineDocs({ dir: 'content/docs', docs: { mdxOptions } });
export const sections = defineCollections({ type: 'doc', dir: 'content/sections', mdxOptions });

// The blog's posts, at /blog/<name> (lib/blog.ts): each names its description and its date, a release's post its tag, whose GitHub release it links, and a post with a byline its author (lib/blog.ts). The fields reuse the schema's own string type, so the site needs no schema library of its own.
const text = frontmatterSchema.shape.title;
export const blog = defineCollections({
  type: 'doc',
  dir: 'content/blog',
  mdxOptions,
  schema: frontmatterSchema.extend({ description: text, date: text.regex(/^\d{4}-\d{2}-\d{2}$/), release: text.optional(), author: text.optional() }),
});

export default defineConfig();
