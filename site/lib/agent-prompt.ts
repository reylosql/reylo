import { readFile } from 'node:fs/promises';

/** Where each prompt for a coding agent is written, in an `<AgentPrompt>` card: Reylo's in Setup, and Reylo Migrate's on its intro. */
export const agentPromptFiles = {
  reylo: 'content/sections/01-setup.mdx',
  migrate: 'content/docs/migrate.mdx',
} as const;

/**
 * The prompt an `<AgentPrompt>` card in `file` holds, as its copy button copies it, so a button on
 * another page (the docs intro's, a landing's) copies the same text. Run at build time.
 */
export async function readAgentPrompt(file: string): Promise<string | undefined> {
  const text = await readFile(file, 'utf8');
  const prompt = text.match(/<AgentPrompt[^>]*>\s*```text\n([\s\S]*?)\n```\s*<\/AgentPrompt>/)?.[1];
  return prompt === undefined ? undefined : `${prompt}\n`;
}
