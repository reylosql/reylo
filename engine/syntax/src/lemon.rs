//! A table-driven LALR(1) parser over tables the Lemon parser generator wrote, building a [`Tree`]. It follows Lemon's own driver (`lempar.c`), with its token fallbacks and wildcard. A statement the grammar refuses still gets a tree. Where one token deleted, or one inserted, lets the next few read, the parser repairs the statement there and reads on, so the rest keeps its structure; otherwise what was read of it, and the tokens up to the next statement, go under an error node, and parsing starts again there.

use crate::lalr::Lexeme;
use crate::tree::{span, Node, NodeId, SyntaxError, Tree, ERROR_KIND};

/// A grammar's tables and the numbers that mark out its kinds of action, as Lemon lays them out.
pub struct Grammar {
    pub action: &'static [u16],
    pub lookahead: &'static [u16],
    pub shift_ofst: &'static [u16],
    pub reduce_ofst: &'static [i16],
    pub default: &'static [u16],
    pub fallback: &'static [u16],
    pub rule_lhs: &'static [u16],
    pub rule_nrhs: &'static [i8],
    pub wildcard: u16,
    pub max_shift: u16,
    pub min_shiftreduce: u16,
    pub max_shiftreduce: u16,
    pub accept: u16,
    pub min_reduce: u16,
    /// A token the scanner could not read, which no state takes, the wildcard's included.
    pub illegal: u16,
    /// How many terminals the grammar has; they are the symbols below this.
    pub ntokens: u16,
    /// A statement's end, where parsing starts over after an error.
    pub separator: u16,
    /// The symbols that hold finished statements, which an error in a later one leaves as they are.
    pub complete: &'static [u16],
    /// Whether a rule's action refuses what the rule read, which the tables cannot say.
    pub refuse: fn(&Reduced<'_>) -> Option<SyntaxError>,
}

/// A rule just reduced, for [`Grammar::refuse`].
pub struct Reduced<'t> {
    pub rule: usize,
    /// The node the rule made.
    pub node: NodeId,
    pub nodes: &'t [Node],
    pub child_ids: &'t [NodeId],
    pub text: &'t str,
}

impl Reduced<'_> {
    /// The children of `id`.
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        let n = &self.nodes[id as usize];
        if n.leaf {
            return &[];
        }
        &self.child_ids[n.lead as usize..(n.lead + n.children) as usize]
    }
}

impl Grammar {
    #[inline]
    fn shift_action(&self, state: u16, mut look: u16) -> u16 {
        if state > self.max_shift {
            return state;
        }
        loop {
            let i = self.shift_ofst[state as usize] as usize + look as usize;
            if self.lookahead[i] == look {
                return self.action[i];
            }
            let fallback = self.fallback[look as usize];
            if fallback != 0 {
                look = fallback;
                continue;
            }
            let j = i - look as usize + self.wildcard as usize;
            if self.lookahead[j] == self.wildcard && look > 0 {
                return self.action[j];
            }
            return self.default[state as usize];
        }
    }

    #[inline]
    fn reduce_action(&self, state: u16, lhs: u16) -> u16 {
        self.action[(self.reduce_ofst[state as usize] as i32 + lhs as i32) as usize]
    }

    /// Whether the parser in `states` takes `sym`: it reduces as the tables say, then shifts it, or accepts at the end. Actions' refusals are not the tables', and are left out.
    fn takes(&self, states: &mut Vec<u16>, sym: u16) -> bool {
        if sym == self.illegal {
            return false;
        }
        let mut act = *states.last().unwrap();
        loop {
            act = self.shift_action(act, sym);
            if act >= self.min_reduce {
                let rule = (act - self.min_reduce) as usize;
                let len = -(self.rule_nrhs[rule] as i32) as usize;
                if len >= states.len() {
                    return false;
                }
                states.truncate(states.len() - len);
                act = self.reduce_action(*states.last().unwrap(), self.rule_lhs[rule]);
                states.push(act);
            } else if act <= self.max_shiftreduce {
                states.push(if act > self.max_shift { act + self.min_reduce - self.min_shiftreduce } else { act });
                return true;
            } else {
                return act == self.accept && sym == 0;
            }
        }
    }

    /// How many of `syms` the parser in `states` reads, in order, before one is refused.
    fn reads(&self, states: &[u16], syms: &[u16]) -> usize {
        let mut states = states.to_vec();
        syms.iter().take_while(|sym| self.takes(&mut states, **sym)).count()
    }

    /// The terminals the parser in `states` takes next, end of input included as 0.
    fn expected(&self, states: &[u16]) -> Vec<u16> {
        (0..self.ntokens).filter(|t| *t != self.illegal && self.reads(states, &[*t]) == 1).collect()
    }

    /// The repair that lets the next tokens read, as [`crate::lalr`] chooses one: an insertion, then a deletion, then a replacement.
    fn repair(&self, states: &[u16], expected: &[u16], tokens: &[Lexeme], p: usize) -> Option<(bool, Option<u16>)> {
        let mut window: Vec<u16> = Vec::with_capacity(5);
        for l in tokens[p.min(tokens.len())..].iter().take(4) {
            window.push(l.sym);
            if l.sym == self.separator {
                break;
            }
        }
        if window.len() < 4 && window.last() != Some(&self.separator) {
            window.push(0);
        }
        let reads_all = |syms: &[u16]| self.reads(states, syms) == syms.len();
        let with = |t: u16, rest: &[u16]| -> Vec<u16> { std::iter::once(t).chain(rest.iter().copied()).collect() };
        let candidates = expected.iter().copied().filter(|t| *t != 0);
        if let Some(t) = candidates.clone().find(|t| reads_all(&with(*t, &window))) {
            return Some((false, Some(t)));
        }
        let deletable = tokens.get(p).is_some_and(|l| l.sym != self.separator);
        if deletable && window.len() > 1 && reads_all(&window[1..]) {
            return Some((true, None));
        }
        if deletable {
            if let Some(t) = candidates.clone().find(|t| reads_all(&with(*t, &window[1..]))) {
                return Some((true, Some(t)));
            }
        }
        None
    }
}

/// The state an entry of the stack stands for: a deleted token's entry stands for the one under it.
const DELETED: u16 = u16::MAX;

/// Parses `tokens`, of `text`, into a tree. Each error's message is left for the dialect to write.
pub fn parse<'s>(g: &Grammar, text: &'s str, tokens: &[Lexeme], tail: u32) -> Tree<'s> {
    let mut nodes: Vec<Node> = Vec::with_capacity(tokens.len() * 3);
    let mut child_ids: Vec<NodeId> = Vec::with_capacity(tokens.len() * 3);
    // Each entry's state, and the node read there; the first entry has none.
    let mut stack: Vec<(u16, NodeId)> = Vec::with_capacity(64);
    stack.push((0, 0));
    let mut errors: Vec<SyntaxError> = Vec::new();
    let mut finished: Vec<NodeId> = Vec::new();
    let mut p = 0usize;
    // A terminal a repair put before the token at `p`, and the repairs made in this statement and in the whole parse.
    let mut inserted: Option<Lexeme> = None;
    let (mut repaired, mut repairs) = (0usize, 0usize);
    let top = |stack: &[(u16, NodeId)]| stack.iter().rev().find(|e| e.0 != DELETED).map_or(0, |e| e.0);
    // The stack as it stood when the parser first read the current token, before the reductions it led to: a default reduction runs without checking the token, so a repair starts from here.
    // Only what reductions overwrite is saved, in reverse, with where it starts: the stack below that is as it was.
    let mut kept: Vec<(u16, NodeId)> = Vec::new();
    let mut kept_from = 0usize;
    // Deleted tokens on top of the stack, set aside while a reduction runs below them.
    let mut above: Vec<(u16, NodeId)> = Vec::new();
    'read: loop {
        let current = inserted.or_else(|| tokens.get(p).copied());
        let sym = current.map_or(0, |l| l.sym);
        let mut act = top(&stack);
        let mut reduced = false;
        // What an action refused while this token was read. Reading stops once the token is taken, and a later refusal replaces an earlier one, as each replaces the parser's one error message.
        let mut refused: Option<SyntaxError> = None;
        loop {
            if sym == g.illegal {
                act = g.max_shiftreduce + 1;
            } else {
                act = g.shift_action(act, sym);
            }
            if act >= g.min_reduce {
                if !reduced {
                    kept.clear();
                    kept_from = stack.len();
                    reduced = true;
                }
                let rule = (act - g.min_reduce) as usize;
                let len = -(g.rule_nrhs[rule] as i32) as usize;
                let lhs = g.rule_lhs[rule];
                // A token a repair deleted on top of the stack comes after this rule's parts, and stays above them for the next rule; one among them goes with them.
                let mut top_end = stack.len();
                while top_end > 1 && stack[top_end - 1].0 == DELETED {
                    top_end -= 1;
                }
                let mut at = top_end;
                let mut counted = 0;
                while counted < len {
                    at -= 1;
                    if stack[at].0 != DELETED {
                        counted += 1;
                    }
                }
                if at < kept_from {
                    kept.extend(stack[at..kept_from].iter().rev());
                    kept_from = at;
                }
                let first = child_ids.len() as u32;
                child_ids.extend(stack[at..top_end].iter().map(|e| e.1));
                let (start, end) = span(&nodes, &child_ids[first as usize..], current.map_or(tail, |l| l.lead));
                let children = (top_end - at) as u32;
                above.clear();
                above.extend_from_slice(&stack[top_end..]);
                stack.truncate(at);
                nodes.push(Node { kind: lhs, start, end, lead: first, children, leaf: false });
                let node = nodes.len() as NodeId - 1;
                if let Some(e) = (g.refuse)(&Reduced { rule, node, nodes: &nodes, child_ids: &child_ids, text }) {
                    refused = Some(e);
                }
                act = g.reduce_action(top(&stack), lhs);
                stack.push((act, node));
                stack.extend_from_slice(&above);
            } else if act <= g.max_shiftreduce && refused.is_none() {
                let Some(l) = current else { break 'read };
                nodes.push(Node { kind: l.sym, start: l.start, end: l.end, lead: l.lead, children: 0, leaf: true });
                let state = if act > g.max_shift { act + g.min_reduce - g.min_shiftreduce } else { act };
                stack.push((state, nodes.len() as NodeId - 1));
                if inserted.take().is_none() {
                    p += 1;
                }
                if l.sym == g.separator {
                    repaired = 0;
                }
                continue 'read;
            } else if act == g.accept && refused.is_none() {
                finished.extend(stack.drain(1..).map(|e| e.1));
                break 'read;
            } else {
                // What was read of the statement, and the tokens to its end, under one error node.
                let end_of_text = text.len() as u32;
                let (from, to) = current.map_or((end_of_text, end_of_text), |l| (l.start, l.end));
                let taken = act <= g.max_shiftreduce || act == g.accept;
                let refusal = refused.take().filter(|_| taken);
                if reduced && refusal.is_none() {
                    stack.truncate(kept_from);
                    stack.extend(kept.iter().rev());
                }
                let states: Vec<u16> = stack.iter().map(|e| e.0).filter(|s| *s != DELETED).collect();
                // Past the parse's budget of 100 errors looked into, an error says what it is and no more.
                let expected = if refusal.is_none() && sym != g.illegal && repairs < 100 { g.expected(&states) } else { Vec::new() };
                repairs += 1;
                // A statement already repaired reports only its first error, as SQLite would.
                if repaired == 0 {
                    errors.push(refusal.clone().unwrap_or_else(|| SyntaxError { message: String::new(), start: from, end: to, expected: expected.clone() }));
                }
                let repair = if refusal.is_some() || sym == g.illegal || inserted.is_some() || repaired >= 4 || repairs >= 100 { None } else { g.repair(&states, &expected, tokens, p) };
                if let Some((delete, insert)) = repair {
                    repaired += 1;
                    if delete {
                        let l = tokens[p];
                        nodes.push(Node { kind: ERROR_KIND, start: l.start, end: l.end, lead: l.lead, children: 0, leaf: true });
                        stack.push((DELETED, nodes.len() as NodeId - 1));
                        p += 1;
                    }
                    if let Some(t) = insert {
                        // The token the repair supplies reads nothing: it sits where the next token's trivia starts.
                        let at = tokens.get(p).map_or(tail, |l| l.lead);
                        inserted = Some(Lexeme { sym: t, lead: at, start: at, end: at });
                    }
                    continue 'read;
                }
                repaired = 0;
                // A token a repair supplied that the parser never took goes with the repair.
                inserted = None;
                let mut held: Vec<NodeId> = stack.drain(1..).map(|e| e.1).collect();
                let done = held.iter().take_while(|id| g.complete.contains(&nodes[**id as usize].kind)).count();
                finished.extend(held.drain(..done));
                while let Some(l) = tokens.get(p) {
                    nodes.push(Node { kind: l.sym, start: l.start, end: l.end, lead: l.lead, children: 0, leaf: true });
                    held.push(nodes.len() as NodeId - 1);
                    p += 1;
                    if l.sym == g.separator {
                        break;
                    }
                }
                if !held.is_empty() {
                    let first_child = child_ids.len() as u32;
                    let (start, end) = span(&nodes, &held, from);
                    child_ids.extend_from_slice(&held);
                    nodes.push(Node { kind: ERROR_KIND, start, end, lead: first_child, children: held.len() as u32, leaf: false });
                    finished.push(nodes.len() as NodeId - 1);
                }
                if p >= tokens.len() {
                    break 'read;
                }
                continue 'read;
            }
        }
    }
    let first = child_ids.len() as u32;
    child_ids.extend_from_slice(&finished);
    let (start, end) = span(&nodes, &finished, tail);
    nodes.push(Node { kind: 0, start, end, lead: first, children: finished.len() as u32, leaf: false });
    let root = nodes.len() as NodeId - 1;
    Tree { text, nodes, child_ids, root, tail, errors }
}
