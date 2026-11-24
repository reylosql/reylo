import { useTheme } from 'fumadocs-ui/provider/base';
import { createContext, use, useCallback, useMemo } from 'react';
import { githubUrl } from '@/lib/layout.shared';
import { REYLO_API } from '@/lib/live/api-docs';
import { PlayApp, type Channel, type PlayData } from '@/lib/live/app';

// The playground inside a blog post, `<PlayEmbed />` in its MDX: the /play page's data (lib/live/play-data.ts), which the
// post's loader reads only for a post that embeds it (app/routes/blog-post.tsx), and /play's worker, started on the
// reader's first edit, so the post downloads Postgres and the engine only for a reader who uses them.
export const PlayDataContext = createContext<PlayData | undefined>(undefined);

const labels = { idle: 'Postgres starts when you edit', loading: 'Starting Postgres 18…', failed: 'Postgres could not start in this browser' };

const about = {
  runs: "Postgres 18 runs in this tab, compiled to WebAssembly (PGlite), holding a sample schema of users, posts and events. Reylo's engine analyzes each query against it, also in this tab: nothing is run, and nothing is sent anywhere.",
};

export function PlayEmbed() {
  const data = use(PlayDataContext);
  const { resolvedTheme, setTheme } = useTheme();
  const connect = useCallback(() => new Worker(new URL('../lib/live/worker.ts', import.meta.url), { type: 'module' }) as unknown as Channel, []);
  const embed = useMemo(() => ({ lazy: true, play: '/play' }), []);
  if (!data) throw new Error('<PlayEmbed /> needs the playground\'s data: its post\'s loader reads it for a post whose MDX embeds it');
  return <PlayApp data={data} connect={connect} theme={{ resolved: resolvedTheme, set: setTheme }} labels={labels} home="/" github={githubUrl} api={REYLO_API} about={about} embed={embed} />;
}
