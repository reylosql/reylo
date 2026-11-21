import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';

// Apart from lib/blog-meta.ts, whose metadata the browser runs: this reads files with Node's APIs, at build time only.

/** The hash of the link preview's route, which its URL carries, so that a change to the card changes the URL and the caches of X and Slack fetch the new card. Read by a loader, at build time. */
export async function ogHash(): Promise<string> {
  return createHash('sha256').update(await readFile('app/routes/opengraph-image.tsx')).digest('hex').slice(0, 16);
}
