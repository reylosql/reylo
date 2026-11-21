// GitHub's alerts, a blockquote that opens `[!NOTE]`, `[!TIP]`, `[!IMPORTANT]`, `[!WARNING]` or `[!CAUTION]`, as the docs' `<Callout>`, so that a release's notes read the same on GitHub and as a blog post (content/blog).
import type { MdNode } from './remark-mapper.ts';

const ALERTS: Record<string, { type: 'info' | 'warning'; title: string }> = {
  NOTE: { type: 'info', title: 'Note' },
  TIP: { type: 'info', title: 'Tip' },
  IMPORTANT: { type: 'warning', title: 'Important' },
  WARNING: { type: 'warning', title: 'Warning' },
  CAUTION: { type: 'warning', title: 'Caution' },
};

const MARKER = /^\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\][ \t]*(?:\r?\n|$)/;

/** The alert a blockquote opens with, and what follows its marker, or nothing for a plain blockquote. */
function alert(quote: MdNode): { type: string; title: string; children: MdNode[] } | undefined {
  const [first, ...after] = quote.children ?? [];
  const lead = first?.type === 'paragraph' ? first.children?.[0] : undefined;
  const match = lead?.type === 'text' ? MARKER.exec(lead.value ?? '') : null;
  if (!first || !lead || !match) return undefined;
  const rest = lead.value!.slice(match[0].length);
  const inline = [...(rest ? [{ ...lead, value: rest }] : []), ...(first.children ?? []).slice(1)];
  return { ...ALERTS[match[1]!]!, children: [...(inline.length ? [{ ...first, children: inline }] : []), ...after] };
}

export function remarkAlerts() {
  return (tree: MdNode) => {
    const visit = (node: MdNode) => {
      node.children?.forEach((child, i) => {
        const found = child.type === 'blockquote' ? alert(child) : undefined;
        if (!found) return visit(child);
        node.children![i] = {
          type: 'mdxJsxFlowElement',
          name: 'Callout',
          attributes: [
            { type: 'mdxJsxAttribute', name: 'type', value: found.type },
            { type: 'mdxJsxAttribute', name: 'title', value: found.title },
          ],
          children: found.children,
        };
      });
    };
    visit(tree);
  };
}
