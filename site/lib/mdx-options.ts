import type { applyMdxPreset } from 'fumadocs-mdx/config';

type Environment = Parameters<ReturnType<typeof applyMdxPreset>>[0];

/**
 * docs-kit's MDX options (each section's Markdown on its heading, Twoslash with its cache and the
 * `// @hover:` directive, the hanging indent of wrapped code, each distinct popup built once), with
 * Reylo's own: the modules Reylo's mapper produced for the page's `.sql` blocks, put above each
 * importing block (lib/remark-mapper.ts), the type descriptor format's cards, rules and TypeScript
 * on its own page (lib/remark-typedefs.ts), Twoslash's options (lib/twoslash.ts), the Inference
 * cards' types and the error examples' errors in each heading's section Markdown
 * (lib/inference-card-markdown.ts, lib/sql-errors.ts), the ```terminal blocks' language (lib/terminal.ts) and the words a line of theirs changes in place (lib/rehype-terminal-diff.ts), GitHub's alerts as callouts (lib/remark-alerts.ts), and each table's declaration on hover in `sql` blocks (lib/table-hovers.ts).
 */
export async function mdxOptions(environment: Environment) {
  const { docsMdxOptions } = await import('docs-kit/mdx');
  const { remarkMapper } = await import('./remark-mapper.ts');
  const { remarkTypedefs } = await import('./remark-typedefs.ts');
  const { remarkAlerts } = await import('./remark-alerts.ts');
  const { reyloTwoslash } = await import('./twoslash.ts');
  const { rehypeInferenceCardSections } = await import('./inference-card-markdown.ts');
  const { rehypeErrorSections } = await import('./sql-errors.ts');
  const { terminal, terminalColors } = await import('./terminal.ts');
  const { rehypeTerminalDiff } = await import('./rehype-terminal-diff.ts');
  // The GitHub themes Fumadocs uses, with the colours of the CLI's own output added for ```terminal blocks.
  const { bundledThemes } = await import('shiki');
  const theme = async (name: 'github-light' | 'github-dark', extra: typeof terminalColors.dark) => {
    const base = (await bundledThemes[name]()).default;
    return { ...base, tokenColors: [...(base.tokenColors ?? []), ...extra] };
  };
  const themes = { light: await theme('github-light', terminalColors.light), dark: await theme('github-dark', terminalColors.dark) };
  const { tableHovers } = await import('./table-hovers.ts');
  const options = await docsMdxOptions(environment, { remarkPlugins: [remarkAlerts, remarkTypedefs, remarkMapper], twoslash: reyloTwoslash(), langs: ['shellscript', 'sql', terminal], themes, transformers: [tableHovers] });
  // After docs-kit's rehypeSections: each inference card's section Markdown carries its types, and
  // each error example its errors.
  return { ...options, rehypePlugins: [...(options.rehypePlugins ?? []), rehypeTerminalDiff, rehypeInferenceCardSections, rehypeErrorSections] };
}
