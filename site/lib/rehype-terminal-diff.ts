/** A hast node, as much of one as this plugin reads and writes. */
type HastNode = { type: string; value?: string; tagName?: string; children?: HastNode[]; properties?: Record<string, unknown> };

const textOf = (node: HastNode): string => (node.type === 'text' ? (node.value ?? '') : (node.children ?? []).map(textOf).join(''));

const hasClass = (node: HastNode, name: string) => {
  const value = node.properties?.class ?? node.properties?.className;
  return (Array.isArray(value) ? value : String(value ?? '').split(' ')).includes(name);
};

/** A word a line changes in place, as the CLI marks it where colour is off: `[-title-]{+heading+}`. */
const MARK = /\[-(.*?)-\]|\{\+(.*?)\+\}/g;

/** A line of a ```terminal block that `reylo` printed changed in place, a column's or a table's: its indent, then what follows its `~`. */
const CHANGED = /^(\s+)~ ((?:column|table) .*)$/;

/**
 * Draws a line `reylo` changed in place (a rename in `migrate new`'s summary) as the CLI draws it in a
 * terminal: its `~` yellow, the old words highlighted red and the new green, without the markers it
 * prints where colour is off, which is how the block is written (app/global.css, `.term-removed`).
 */
export function rehypeTerminalDiff() {
  const visit = (node: HastNode) => {
    for (const child of node.children ?? []) {
      // A block may sit inside an MDX component, which is not an element.
      if (!child.children) continue;
      const changed = child.type === 'element' && hasClass(child, 'line') ? CHANGED.exec(textOf(child)) : null;
      if (changed) {
        const text = changed[2];
        const parts: HastNode[] = [{ type: 'text', value: changed[1] }, { type: 'element', tagName: 'span', properties: { className: ['term-changed'] }, children: [{ type: 'text', value: '~' }] }, { type: 'text', value: ' ' }];
        let at = 0;
        for (const m of text.matchAll(MARK)) {
          if (m.index > at) parts.push({ type: 'text', value: text.slice(at, m.index) });
          const removed = m[1] !== undefined;
          parts.push({ type: 'element', tagName: 'span', properties: { className: [removed ? 'term-removed' : 'term-added'] }, children: [{ type: 'text', value: removed ? m[1] : m[2] }] });
          at = m.index + m[0].length;
        }
        if (at < text.length) parts.push({ type: 'text', value: text.slice(at) });
        child.children = parts;
        continue;
      }
      visit(child);
    }
  };
  return (tree: HastNode) => visit(tree);
}
