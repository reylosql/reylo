//! A table-driven LALR(1) parser over tables Bison generated, building a [`Tree`]. A statement the
//! grammar refuses still gets a tree. Where one token deleted, or one inserted, lets the next few
//! read, the parser repairs the statement there and reads on, so the rest keeps its structure;
//! otherwise what was read of it, and the tokens up to the next statement, go under an error node,
//! and parsing starts again there.

use crate::tree::{span, Node, NodeId, SyntaxError, Tree, ERROR_KIND};

/// A grammar's tables, as Bison lays them out.
pub struct Tables {
    pub pact: Vec<i32>,
    pub pgoto: Vec<i32>,
    pub defact: Vec<i32>,
    pub defgoto: Vec<i32>,
    pub table: Vec<i32>,
    pub check: Vec<i32>,
    pub r1: Vec<i32>,
    pub r2: Vec<i32>,
    pub pact_ninf: i32,
    pub table_ninf: i32,
    pub final_state: i32,
    pub last: i32,
    pub ntokens: i32,
}

impl Tables {
    /// Reads the tables from little-endian integers, laid out as `segments` says, in the order of [`Tables`]'s fields.
    pub fn read(bytes: &[u8], segments: &[(usize, usize, usize); 8], pact_ninf: i32, table_ninf: i32, final_state: i32, last: i32, ntokens: i32) -> Tables {
        let read = |i: usize| -> Vec<i32> {
            let (start, len, width) = segments[i];
            bytes[start..start + len * width]
                .chunks_exact(width)
                .map(|c| if width == 4 { i32::from_le_bytes([c[0], c[1], c[2], c[3]]) } else { i16::from_le_bytes([c[0], c[1]]) as i32 })
                .collect()
        };
        Tables { pact: read(0), pgoto: read(1), defact: read(2), defgoto: read(3), table: read(4), check: read(5), r1: read(6), r2: read(7), pact_ninf, table_ninf, final_state, last, ntokens }
    }

    /// The action in `state` on the symbol `sym`.
    #[inline]
    fn action(&self, state: i32, sym: i32) -> Action {
        let base = self.pact[state as usize];
        if base != self.pact_ninf {
            let at = base + sym;
            if (0..=self.last).contains(&at) && self.check[at as usize] == sym {
                let act = self.table[at as usize];
                return match act {
                    a if a > 0 => Action::Shift(a),
                    a if a == self.table_ninf => Action::Error,
                    0 => Action::Error,
                    a => Action::Reduce(-a as usize),
                };
            }
        }
        match self.defact[state as usize] {
            0 => Action::Error,
            r => Action::Reduce(r as usize),
        }
    }

    /// Whether `state` takes its default reduction whatever comes next.
    #[inline]
    fn default_only(&self, state: i32) -> bool {
        self.pact[state as usize] == self.pact_ninf
    }

    #[inline]
    fn goto(&self, top: i32, lhs: i32) -> i32 {
        let i = (lhs - self.ntokens) as usize;
        let at = self.pgoto[i] + top;
        if (0..=self.last).contains(&at) && self.check[at as usize] == top {
            self.table[at as usize]
        } else {
            self.defgoto[i]
        }
    }
}

/// How many tokens past a repair must read for the repair to stand.
const REPAIR_WINDOW: usize = 4;
/// How many repairs one statement may take before the rest of it goes under an error node.
const REPAIRS_PER_STATEMENT: usize = 4;
/// How many errors one parse looks into, for what would have fit and a repair, so that text that is mostly not SQL costs little more than SQL.
const REPAIRS_PER_PARSE: usize = 100;

impl Tables {
    /// Whether the parser in `states` takes `sym`: it reduces as the tables say, then shifts it, or accepts at the end.
    fn takes(&self, states: &mut Vec<i32>, sym: i32) -> bool {
        loop {
            let state = *states.last().unwrap();
            if state == self.final_state {
                return sym == 0;
            }
            if states.len() >= MAX_STATES {
                return false;
            }
            let look = if self.default_only(state) { 0 } else { sym };
            match self.action(state, look) {
                Action::Shift(next) => {
                    states.push(next);
                    if look == sym || self.default_only(state) && sym == 0 {
                        return true;
                    }
                }
                Action::Reduce(rule) => {
                    let len = self.r2[rule] as usize;
                    states.truncate(states.len() - len);
                    let top = *states.last().unwrap();
                    states.push(self.goto(top, self.r1[rule]));
                }
                Action::Error => return false,
            }
        }
    }

    /// How many of `syms` the parser in `states` reads, in order, before one is refused.
    fn reads(&self, states: &[i32], syms: &[i32]) -> usize {
        let mut states = states.to_vec();
        syms.iter().take_while(|sym| self.takes(&mut states, **sym)).count()
    }

    /// The terminals the parser in `states` takes next, end of input included as 0.
    pub fn expected(&self, states: &[i32]) -> Vec<u16> {
        // 1 and 2 are Bison's `error` and `$undefined`, which no text spells.
        (0..self.ntokens).filter(|t| *t != 1 && *t != 2 && self.reads(states, &[*t]) == 1).map(|t| t as u16).collect()
    }
}

/// A repair at an error: insert a terminal before the token there, delete that token, or both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Repair {
    Insert(i32),
    Delete,
    Replace(i32),
}

enum Action {
    Shift(i32),
    Reduce(usize),
    Error,
}

/// A token as the parser reads it: `lead..start` is the trivia before it.
#[derive(Clone, Copy, Debug)]
pub struct Lexeme {
    pub sym: u16,
    pub lead: u32,
    pub start: u32,
    pub end: u32,
}

/// How many states Bison's parser holds before it gives up with "memory exhausted" (`YYMAXDEPTH`, whose default the grammars keep). A statement's nesting reaches it without any node to show for it, as parentheses around one expression do.
const MAX_STATES: usize = 10_000;

/// A token the scanner could not get past: asking for it ends the parse there.
pub const SCAN_STOP: u16 = u16::MAX;

/// What a grammar's rules do beside building the tree: a rule's semantic action.
pub trait Actions {
    type Value;
    /// A shifted token's value; `index` is its place in the token list.
    fn token(&mut self, index: usize) -> Self::Value;
    /// A reduction of `rule` over `values`, its right-hand side, with where each starts.
    /// `last_read` is the last token the parser has asked for, which is where an error points.
    fn reduce(&mut self, rule: usize, values: &mut [Self::Value], locations: &[i32], here: i32, last_read: Option<usize>) -> Result<Self::Value, String>;
    /// Whether a statement the grammar refuses is repaired and read on, which only a tree needs: a parse for a value stops at the error anyway.
    fn recovers(&self) -> bool {
        true
    }
    /// The error when the parser's stack is full, with the last token asked for.
    fn exhausted(&mut self, _last_read: Option<usize>) -> String {
        EXHAUSTED.to_string()
    }
}

/// What Bison says when its stack reaches `YYMAXDEPTH`.
pub const EXHAUSTED: &str = "memory exhausted";

/// Actions that keep nothing: a parse for its tree alone.
pub struct NoActions;

impl Actions for NoActions {
    type Value = ();
    fn token(&mut self, _: usize) {}
    fn reduce(&mut self, _: usize, _: &mut [()], _: &[i32], _: i32, _: Option<usize>) -> Result<(), String> {
        Ok(())
    }
}

/// How a parse ended.
pub struct Parsed<'s, V> {
    pub tree: Tree<'s>,
    /// The value of the whole text, where it followed the grammar and every action held.
    pub value: Option<V>,
    /// Where an action refused the text, with why.
    pub refused: Option<String>,
    /// The token that refusal points at.
    pub refused_at: Option<(u32, u32)>,
    /// Whether the parse ended by asking for the token the scanner could not get past.
    pub scan_stopped: bool,
}

/// Parses `tokens`, of `text`, into a tree, running `actions` on each reduction. `separator`, a
/// statement's end, is where parsing starts over after an error, and `complete` are the symbols
/// that hold the statements before it.
pub fn parse<'s, A: Actions>(tables: &Tables, text: &'s str, tokens: &[Lexeme], tail: u32, separator: u16, complete: &[u16], actions: &mut A) -> Parsed<'s, A::Value> {
    let mut nodes: Vec<Node> = Vec::with_capacity(tokens.len() * 3);
    let mut child_ids: Vec<NodeId> = Vec::with_capacity(tokens.len() * 3);
    let mut states: Vec<i32> = Vec::with_capacity(64);
    states.push(0);
    let mut trees: Vec<NodeId> = Vec::with_capacity(64);
    let mut values: Vec<A::Value> = Vec::with_capacity(64);
    let mut locations: Vec<i32> = Vec::with_capacity(64);
    let mut errors: Vec<SyntaxError> = Vec::new();
    let mut finished: Vec<NodeId> = Vec::new();
    let mut p = 0usize;
    // A terminal a repair put before the token at `p`, read before it.
    let mut inserted: Option<Lexeme> = None;
    // The repairs made in the statement being read, and in the whole parse.
    let (mut repaired, mut repairs) = (0usize, 0usize);
    let token = |p: usize, inserted: &Option<Lexeme>| -> Option<Lexeme> { inserted.or_else(|| tokens.get(p).copied()) };
    // The parser as it stood when it first read the token at `p`, before the reductions that token
    // led to: a default reduction runs without checking the token, so at an error the parser may
    // stand in a state the token never belonged in. A repair starts from here.
    let recovers = actions.recovers();
    let (mut kept_states, mut kept_trees): (Vec<i32>, Vec<NodeId>) = (Vec::new(), Vec::new());
    let mut kept = false;
    let mut value = None;
    let mut refused = None;
    let mut refused_at = None;
    let mut acting = true;
    let mut scan_stopped = false;
    // Whether the token at `p` has been asked for yet.
    let mut peeked = false;
    loop {
        let state = *states.last().unwrap();
        if state == tables.final_state {
            finished.extend(trees.drain(..));
            if acting {
                value = values.pop();
            }
            break;
        }
        let full = states.len() >= MAX_STATES;
        if full && acting {
            refused = Some(actions.exhausted(if peeked { Some(p) } else { p.checked_sub(1) }));
            acting = false;
            values.clear();
            locations.clear();
        }
        let sym = if full || tables.default_only(state) {
            0
        } else {
            peeked = true;
            token(p, &inserted).map_or(0, |l| l.sym as i32)
        };
        let action = if full {
            Action::Error
        } else if sym == SCAN_STOP as i32 {
            if acting {
                scan_stopped = true;
            }
            Action::Error
        } else {
            tables.action(state, sym)
        };
        match action {
            Action::Shift(next) => {
                if let Some(l) = token(p, &inserted) {
                    nodes.push(Node { kind: l.sym, start: l.start, end: l.end, lead: l.lead, children: 0, leaf: true });
                    trees.push(nodes.len() as NodeId - 1);
                    if acting {
                        values.push(actions.token(p));
                        locations.push(l.start as i32);
                    }
                    if inserted.take().is_none() {
                        p += 1;
                    }
                    if l.sym == separator {
                        repaired = 0;
                    }
                }
                peeked = false;
                kept = false;
                states.push(next);
            }
            Action::Reduce(rule) => {
                if recovers && !kept {
                    kept_states.clear();
                    kept_states.extend_from_slice(&states);
                    kept_trees.clear();
                    kept_trees.extend_from_slice(&trees);
                    kept = true;
                }
                let len = tables.r2[rule] as usize;
                let lhs = tables.r1[rule];
                // A token a repair deleted sits among the rule's parts, and goes with them.
                let mut from = trees.len();
                let mut counted = 0;
                while counted < len {
                    from -= 1;
                    if nodes[trees[from] as usize].kind != ERROR_KIND {
                        counted += 1;
                    }
                }
                let first = child_ids.len() as u32;
                child_ids.extend_from_slice(&trees[from..]);
                let (start, end) = span(&nodes, &trees[from..], token(p, &inserted).map_or(tail, |l| l.lead));
                let children = (trees.len() - from) as u32;
                trees.truncate(from);
                states.truncate(states.len() - len);
                nodes.push(Node { kind: lhs as u16, start, end, lead: first, children, leaf: false });
                trees.push(nodes.len() as NodeId - 1);
                if acting {
                    let at = values.len() - len;
                    // Bison's default for this grammar: where the first part that read anything starts.
                    let location = locations[locations.len() - len..].iter().copied().find(|l| *l >= 0).unwrap_or(-1);
                    let last_read = if peeked { Some(p) } else { p.checked_sub(1) };
                    match actions.reduce(rule, &mut values[at..], &locations[locations.len() - len..], location, last_read) {
                        Ok(v) => {
                            values.truncate(at);
                            locations.truncate(locations.len() - len);
                            values.push(v);
                            locations.push(location);
                        }
                        Err(message) => {
                            refused = Some(message);
                            refused_at = last_read.and_then(|i| tokens.get(i)).map(|l| (l.start, l.end));
                            acting = false;
                            values.clear();
                            locations.clear();
                        }
                    }
                }
                let top = *states.last().unwrap();
                states.push(tables.goto(top, lhs));
            }
            Action::Error => {
                let at = if full && !peeked { p.checked_sub(1).and_then(|i| tokens.get(i)).copied() } else { token(p, &inserted) };
                let end_of_text = text.len() as u32;
                let (from, to) = at.map_or((end_of_text, end_of_text), |l| (l.start, l.end));
                if kept {
                    std::mem::swap(&mut states, &mut kept_states);
                    std::mem::swap(&mut trees, &mut kept_trees);
                    kept = false;
                }
                // Past the parse's budget of repairs, an error says what it is and no more.
                let expected = if full || sym == SCAN_STOP as i32 || !recovers || repairs >= REPAIRS_PER_PARSE { Vec::new() } else { tables.expected(&states) };
                repairs += 1;
                // Bison's own limit, at the last token read. Any other error's words are the caller's. A statement already repaired reports only its first error, as its database would.
                let message = if full { EXHAUSTED.to_string() } else { String::new() };
                if repaired == 0 {
                    errors.push(SyntaxError { message, start: from, end: to, expected: expected.clone() });
                }
                acting = false;
                values.clear();
                locations.clear();
                peeked = false;
                let repair = if full || sym == SCAN_STOP as i32 || !recovers || inserted.is_some() || repaired >= REPAIRS_PER_STATEMENT || repairs >= REPAIRS_PER_PARSE {
                    None
                } else {
                    choose_repair(tables, &states, &expected, tokens, p, separator)
                };
                if let Some(repair) = repair {
                    repaired += 1;
                    let here = tokens.get(p).map_or(tail, |l| l.lead);
                    if matches!(repair, Repair::Delete | Repair::Replace(_)) {
                        let l = tokens[p];
                        nodes.push(Node { kind: ERROR_KIND, start: l.start, end: l.end, lead: l.lead, children: 0, leaf: true });
                        trees.push(nodes.len() as NodeId - 1);
                        p += 1;
                    }
                    if let Repair::Insert(t) | Repair::Replace(t) = repair {
                        // The token the repair supplies reads nothing: it sits where the next token's trivia starts.
                        let at = if matches!(repair, Repair::Replace(_)) { tokens.get(p).map_or(tail, |l| l.lead) } else { here };
                        inserted = Some(Lexeme { sym: t as u16, lead: at, start: at, end: at });
                    }
                    continue;
                }
                repaired = 0;
                let mut held: Vec<NodeId> = std::mem::take(&mut trees);
                // The statements already read, and the `;` after them, stay as they are.
                let done = held
                    .iter()
                    .take_while(|id| {
                        let n = &nodes[**id as usize];
                        complete.contains(&n.kind) || (n.leaf && n.kind == separator)
                    })
                    .count();
                // A list the error's lookahead reduced ends in the statement the error is in: its earlier statements stand, and that one goes with the error.
                let open = done.checked_sub(1).map(|last| held[last]).filter(|id| complete.contains(&nodes[*id as usize].kind));
                if let Some(mut node) = open {
                    finished.extend(held.drain(..done - 1));
                    while complete.contains(&nodes[node as usize].kind) {
                        let n = nodes[node as usize];
                        match child_ids[n.lead as usize..(n.lead + n.children) as usize] {
                            [one] => node = one,
                            [earlier, sep, last] if nodes[sep as usize].leaf && nodes[sep as usize].kind == separator => {
                                finished.extend([earlier, sep]);
                                node = last;
                                break;
                            }
                            _ => break,
                        }
                    }
                    held[0] = node;
                } else {
                    finished.extend(held.drain(..done));
                }
                while let Some(l) = tokens.get(p) {
                    nodes.push(Node { kind: l.sym, start: l.start, end: l.end, lead: l.lead, children: 0, leaf: true });
                    held.push(nodes.len() as NodeId - 1);
                    p += 1;
                    if l.sym == separator {
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
                states.truncate(1);
                if p >= tokens.len() {
                    break;
                }
            }
        }
    }
    let first = child_ids.len() as u32;
    child_ids.extend_from_slice(&finished);
    let (start, end) = span(&nodes, &finished, tail);
    nodes.push(Node { kind: 0, start, end, lead: first, children: finished.len() as u32, leaf: false });
    let root = nodes.len() as NodeId - 1;
    Parsed { tree: Tree { text, nodes, child_ids, root, tail, errors }, value, refused, refused_at, scan_stopped }
}

/// The repair that lets the most of the next [`REPAIR_WINDOW`] tokens read, where one lets them all: an insertion first, then a deletion, then a replacement, and among insertions the one the tables list first. The window stops at the statement's end, and a statement's end is never deleted.
fn choose_repair(tables: &Tables, states: &[i32], expected: &[u16], tokens: &[Lexeme], p: usize, separator: u16) -> Option<Repair> {
    let mut window: Vec<i32> = Vec::with_capacity(REPAIR_WINDOW + 1);
    for l in tokens[p.min(tokens.len())..].iter().take(REPAIR_WINDOW) {
        window.push(l.sym as i32);
        if l.sym == separator {
            break;
        }
    }
    // The end of the text reads as end of input, which an accepted statement takes.
    if window.len() < REPAIR_WINDOW && window.last() != Some(&(separator as i32)) {
        window.push(0);
    }
    let reads_all = |syms: &[i32]| tables.reads(states, syms) == syms.len();
    let with = |t: i32, rest: &[i32]| -> Vec<i32> { std::iter::once(t).chain(rest.iter().copied()).collect() };
    let candidates = expected.iter().map(|t| *t as i32).filter(|t| *t != 0);
    if let Some(t) = candidates.clone().find(|t| reads_all(&with(*t, &window))) {
        return Some(Repair::Insert(t));
    }
    let deletable = tokens.get(p).is_some_and(|l| l.sym != separator);
    if deletable && window.len() > 1 && reads_all(&window[1..]) {
        return Some(Repair::Delete);
    }
    if deletable {
        if let Some(t) = candidates.clone().find(|t| reads_all(&with(*t, &window[1..]))) {
            return Some(Repair::Replace(t));
        }
    }
    None
}
