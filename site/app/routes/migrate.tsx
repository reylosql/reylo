import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import type { Route } from './+types/migrate';
import { MigrateLanding } from '@/components/migrate-landing';
import { agentPromptFiles, readAgentPrompt } from '@/lib/agent-prompt';
import { siteUrl } from '@/lib/layout.shared';
import { migrateOgAlt, ogSize } from '@/lib/og';

// Reylo Migrate's landing page (components/migrate-landing.tsx); its docs are at /migrate/docs. Its link
// preview is app/routes/migrate-opengraph-image.tsx.
export const meta: Route.MetaFunction = ({ loaderData }) => {
  const title = 'Reylo Migrate — SQL-first schema modeling and migrations for people who ship';
  const description = 'SQL-first schema modeling and migrations. Unlock the full power of your database.';
  const url = `${siteUrl}/migrate`;
  const image = `${siteUrl}/migrate/opengraph-image.png?${loaderData?.og ?? ''}`;
  return [
    { title },
    { name: 'description', content: description },
    { tagName: 'link', rel: 'canonical', href: url },
    { tagName: 'link', rel: 'alternate', type: 'text/markdown', href: `${siteUrl}/migrate.md` },
    { property: 'og:title', content: title },
    { property: 'og:description', content: description },
    { property: 'og:url', content: url },
    { property: 'og:site_name', content: 'Reylo' },
    { property: 'og:locale', content: 'en_US' },
    { property: 'og:image', content: image },
    { property: 'og:image:type', content: 'image/png' },
    { property: 'og:image:width', content: String(ogSize.width) },
    { property: 'og:image:height', content: String(ogSize.height) },
    { property: 'og:image:alt', content: migrateOgAlt },
    { property: 'og:type', content: 'website' },
    { name: 'twitter:card', content: 'summary_large_image' },
    { name: 'twitter:creator', content: '@colinhacks' },
    { name: 'twitter:title', content: title },
    { name: 'twitter:description', content: description },
    { name: 'twitter:image', content: image },
    { name: 'twitter:image:alt', content: migrateOgAlt },
    { name: 'twitter:image:type', content: 'image/png' },
    { name: 'twitter:image:width', content: String(ogSize.width) },
    { name: 'twitter:image:height', content: String(ogSize.height) },
  ];
};

/**
 * Run at build time, when the page is rendered to a file (react-router.config.ts): the hash of the link
 * preview's route, which its URL carries, so that a change to the card changes the URL and the caches
 * of X and Slack fetch the new card; Reylo's star count, for the nav's GitHub link, or none when
 * GitHub can't be reached; and the setup prompt the hero's "Copy agent prompt" copies.
 */
export async function loader() {
  const agentPrompt = await readAgentPrompt(agentPromptFiles.migrate);
  const og = createHash('sha256').update(await readFile('app/routes/migrate-opengraph-image.tsx')).digest('hex').slice(0, 16);
  try {
    const res = await fetch('https://api.github.com/repos/reylosql/reylo', { headers: { Accept: 'application/vnd.github+json' } });
    const repo = (await res.json()) as { stargazers_count?: number };
    return { og, agentPrompt, stars: res.ok ? repo.stargazers_count : undefined };
  } catch {
    return { og, agentPrompt, stars: undefined };
  }
}

export default function Migrate({ loaderData }: Route.ComponentProps) {
  return <MigrateLanding stars={loaderData.stars} agentPrompt={loaderData.agentPrompt} />;
}
