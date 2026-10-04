//! The scan's tests against id's own: the one walk a scanline of
//! [`EdgeState::scan_edges`] held to a literal transcription of id's
//! `r_edge.c` scan — `R_ScanEdges`, `R_InsertNewEdges`,
//! `R_GenerateSpans`, `R_LeadingEdge`, `R_TrailingEdge`, `R_CleanupSpan`,
//! `R_RemoveEdges` with its `removeedges[]`/`nextremove` lists, and
//! `R_StepActiveU` with the tail stepped and the aftertail stop — over random
//! edge tables and over real maps' tables, line by line: the spans in emission
//! order, the active edge table (order and every `u`) after every scanline,
//! and every surface's stack links, `last_u` and `spanstate`.
//!
//! The walks part only where an edge runs past `edge_tail`, which a frame's
//! edges never do (`scan_edges`' note); a table built to do it shows where.
//! `QUAKE_SCAN_TABLES` and `QUAKE_SCAN_VIEWS` set how many random tables and
//! how many views of each map are tried (20,000 and 40 unless set; the round
//! that wrote this ran 300,000 and 1,000: no difference).

use super::*;
use crate::render::{Camera, Scene};

// ---------------------------------------------------------------------------
// id's scan, literally: pointers are indices, NULL is CNULL.
// ---------------------------------------------------------------------------

const CNULL: usize = usize::MAX;
const C_HEAD: usize = 0;
const C_TAIL: usize = 1;
const C_AFTERTAIL: usize = 2;
const C_SENTINEL: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq)]
struct CEdge {
    u: i64,
    u_step: i64,
    prev: usize,
    next: usize,
    surfs: [usize; 2],
    nextremove: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CSurf {
    next: usize,
    prev: usize,
    spans: usize,
    key: i32,
    last_u: i32,
    spanstate: i32,
    insubmodel: bool,
    d_ziorigin: f32,
    d_zistepu: f32,
    d_zistepv: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CSpan {
    u: i32,
    v: i32,
    count: i32,
    pnext: usize,
}

/// What a scan reached (id's side), for the report: which cases a table set
/// exercises.
#[derive(Default, Debug, Clone, Copy)]
pub(super) struct Reached {
    pub tables: u64,
    pub lines: u64,
    pub spans: u64,
    pub inserted: u64,
    pub removed: u64,
    /// Removed on the very line it was inserted (`v2 == v`).
    pub removed_on_first_line: u64,
    /// `R_StepActiveU` moved an edge back (its stepped `u` below the one before).
    pub pushbacks: u64,
    /// ... further back than just before its predecessor (the walk-back loop ran).
    pub pushback_walked: u64,
    /// ... to right after `edge_head`.
    pub pushback_to_head: u64,
    /// ... past the place an edge removed on this line had in the line's table
    /// (id removed it before stepping; the fused walk removed it when reached).
    pub pushback_past_removed: u64,
    /// ... whose successor in the line's table ends on this line (the fused walk
    /// relinks a to-be-removed edge's `prev`, and resumes at it).
    pub pushback_next_removed: u64,
    /// ... whose predecessor in the line's table ends on this line.
    pub pushback_prev_removed: u64,
    /// ... walking back past an edge already moved back on the same line.
    pub pushback_past_pushed: u64,
    /// A stepped edge equal to its predecessor's `u` (no move).
    pub ties: u64,
    /// `R_LeadingEdge` sorted two brush-model surfaces of one key on 1/z.
    pub zi_sorts: u64,
    /// An active edge with surfaces on both sides (a cached edge).
    pub both_sides: u64,
    /// id pushed `edge_tail` back (an edge right of the screen).
    pub tail_pushed: u64,
    /// id's walk-back went past `edge_head` (NULL: undefined in the C).
    pub head_ub: u64,
}

impl Reached {
    pub fn add(&mut self, o: &Reached) {
        macro_rules! sum { ($($f:ident),*) => { $( self.$f += o.$f; )* } }
        sum!(tables, lines, spans, inserted, removed, removed_on_first_line, pushbacks, pushback_walked,
            pushback_to_head, pushback_past_removed, pushback_next_removed, pushback_prev_removed,
            pushback_past_pushed, ties, zi_sorts, both_sides, tail_pushed, head_ub);
    }
}

/// One scanline's state as a snapshot: the active edges from `edge_head.next`
/// to `edge_tail` (index, `u`), and each surface's (next, prev, last_u,
/// spanstate).
type Snap = (Vec<(usize, i64)>, Vec<(usize, usize, i32, i32)>);
/// A scan's spans as (v, u, count, surface), in the order they are made.
type Spans = Vec<(i32, i32, i32, usize)>;
/// The port's three-walk scan's spans (u, count, surface) and its last table (edge, u).
type Reference = (Vec<(i32, i32, i32)>, Vec<(usize, i64)>);

struct CScan {
    w: usize,
    h: usize,
    edges: Vec<CEdge>,
    surfs: Vec<CSurf>,
    spans: Vec<CSpan>,
    /// Every span as it was made: (v, u, count, surface).
    emitted: Vec<(i32, i32, i32, usize)>,
    newedges: Vec<usize>,
    removeedges: Vec<usize>,
    current_iv: i32,
    fv: f32,
    edge_head_u_shift20: i32,
    edge_tail_u_shift20: i32,
    r_bmodelactive: i32,
    reached: Reached,
    ub: bool,
    snaps: Vec<Snap>,
    // per-line bookkeeping for `reached`
    line_pos: Vec<usize>,
    pushed_this_line: Vec<bool>,
}

impl CScan {
    fn emit(&mut self, surf: usize, u: i32, count: i32) {
        let pnext = self.surfs[surf].spans;
        self.spans.push(CSpan { u, v: self.current_iv, count, pnext });
        self.surfs[surf].spans = self.spans.len() - 1;
        self.emitted.push((self.current_iv, u, count, surf));
    }

    /// R_InsertNewEdges (the unrolled search is this loop).
    fn insert_new_edges(&mut self, mut edgestoadd: usize, mut edgelist: usize) {
        loop {
            let next_edge = self.edges[edgestoadd].next;
            while self.edges[edgelist].u < self.edges[edgestoadd].u {
                edgelist = self.edges[edgelist].next;
                if edgelist == CNULL {
                    // past edge_sentinel (whose 2000 << 24 wraps negative): NULL
                    self.reached.head_ub += 1;
                    self.ub = true;
                    return;
                }
            }
            // addedge:
            self.edges[edgestoadd].next = edgelist;
            self.edges[edgestoadd].prev = self.edges[edgelist].prev;
            let p = self.edges[edgelist].prev;
            self.edges[p].next = edgestoadd;
            self.edges[edgelist].prev = edgestoadd;
            self.reached.inserted += 1;
            edgestoadd = next_edge;
            if edgestoadd == CNULL {
                break;
            }
        }
    }

    /// R_RemoveEdges.
    fn remove_edges(&mut self, mut pedge: usize) {
        loop {
            let (prev, next) = (self.edges[pedge].prev, self.edges[pedge].next);
            self.edges[next].prev = prev;
            self.edges[prev].next = next;
            self.reached.removed += 1;
            pedge = self.edges[pedge].nextremove;
            if pedge == CNULL {
                break;
            }
        }
    }

    /// R_StepActiveU (the unrolled walk is this loop).
    fn step_active_u(&mut self, mut pedge: usize, removed_here: &[bool]) {
        let mut guard = 4 * self.edges.len() + 16;
        loop {
            guard -= 1;
            assert!(guard > 0, "R_StepActiveU does not end");
            if pedge == CNULL || pedge == C_SENTINEL {
                // walked off the end of the list: undefined in the C
                self.reached.head_ub += 1;
                self.ub = true;
                return;
            }
            // nextedge:
            self.edges[pedge].u += self.edges[pedge].u_step;
            let prev = self.edges[pedge].prev;
            if self.edges[pedge].u >= self.edges[prev].u {
                if self.edges[pedge].u == self.edges[prev].u && pedge >= FIRST_EDGE as usize {
                    self.reached.ties += 1;
                }
                pedge = self.edges[pedge].next;
                continue;
            }
            // pushback:
            if pedge == C_AFTERTAIL {
                return;
            }
            if pedge == C_TAIL {
                self.reached.tail_pushed += 1;
            } else {
                self.reached.pushbacks += 1;
            }
            let pnext_edge = self.edges[pedge].next;
            // pull the edge out of the edge list
            let (p, n) = (self.edges[pedge].prev, self.edges[pedge].next);
            self.edges[n].prev = p;
            self.edges[p].next = n;
            // find out where the edge goes in the edge list
            let mut pwedge = self.edges[self.edges[pedge].prev].prev;
            let mut walked = false;
            let mut past_pushed = false;
            loop {
                if pwedge == CNULL {
                    // id dereferences NULL here
                    self.reached.head_ub += 1;
                    self.ub = true;
                    return;
                }
                if self.edges[pwedge].u > self.edges[pedge].u {
                    if self.pushed_this_line.get(pwedge).copied().unwrap_or(false) {
                        past_pushed = true;
                    }
                    pwedge = self.edges[pwedge].prev;
                    walked = true;
                } else {
                    break;
                }
            }
            if pedge >= FIRST_EDGE as usize {
                let pos = |e: usize| if e == C_HEAD { 0 } else { self.line_pos[e] };
                let (lo, hi) = (pos(pwedge), pos(pedge));
                let between_removed = (0..self.edges.len())
                    .any(|d| removed_here[d] && self.line_pos[d] != usize::MAX && lo < self.line_pos[d] && self.line_pos[d] < hi);
                self.reached.pushback_walked += u64::from(walked);
                self.reached.pushback_to_head += u64::from(pwedge == C_HEAD);
                self.reached.pushback_past_removed += u64::from(between_removed);
                self.reached.pushback_past_pushed += u64::from(past_pushed);
                let at = |k: usize| (0..self.edges.len()).find(|&e| self.line_pos[e] == k);
                let hi_pos = self.line_pos[pedge];
                if let Some(e) = at(hi_pos + 1) {
                    self.reached.pushback_next_removed += u64::from(removed_here[e]);
                }
                if let Some(e) = at(hi_pos - 1) {
                    self.reached.pushback_prev_removed += u64::from(removed_here[e]);
                }
                self.pushed_this_line[pedge] = true;
            }
            // put the edge back into the edge list
            self.edges[pedge].next = self.edges[pwedge].next;
            self.edges[pedge].prev = pwedge;
            let nn = self.edges[pedge].next;
            self.edges[nn].prev = pedge;
            self.edges[pwedge].next = pedge;
            pedge = pnext_edge;
            if pedge == C_TAIL {
                return;
            }
        }
    }

    /// R_CleanupSpan.
    fn cleanup_span(&mut self) {
        let mut surf = self.surfs[1].next;
        let iu = self.edge_tail_u_shift20;
        if iu > self.surfs[surf].last_u {
            let u = self.surfs[surf].last_u;
            self.emit(surf, u, iu - u);
        }
        let mut guard = self.surfs.len() + 2;
        loop {
            self.surfs[surf].spanstate = 0;
            surf = self.surfs[surf].next;
            guard -= 1;
            if surf == 1 || guard == 0 {
                break;
            }
        }
    }

    /// R_TrailingEdge.
    fn trailing_edge(&mut self, surf: usize, edge: usize) {
        self.surfs[surf].spanstate -= 1;
        if self.surfs[surf].spanstate == 0 {
            if self.surfs[surf].insubmodel {
                self.r_bmodelactive -= 1;
            }
            if surf == self.surfs[1].next {
                let iu = (self.edges[edge].u >> 20) as i32;
                if iu > self.surfs[surf].last_u {
                    let u = self.surfs[surf].last_u;
                    self.emit(surf, u, iu - u);
                }
                let below = self.surfs[surf].next;
                self.surfs[below].last_u = iu;
            }
            let (p, n) = (self.surfs[surf].prev, self.surfs[surf].next);
            self.surfs[p].next = n;
            self.surfs[n].prev = p;
        }
    }

    /// The 1/z test of R_LeadingEdge for two brush-model surfaces of one key:
    /// `Some(true)` for "goto newtop/gotposition".
    fn zi_in_front(&mut self, surf: usize, surf2: usize, edge: usize) -> bool {
        self.reached.zi_sorts += 1;
        let s = self.surfs[surf];
        let t = self.surfs[surf2];
        let fu = (self.edges[edge].u.wrapping_sub(0xFFFFF) as f32) as f64 * (1.0 / 1_048_576.0);
        let newzi = (s.d_ziorigin + self.fv * s.d_zistepv) as f64 + fu * s.d_zistepu as f64;
        let newzibottom = newzi * 0.99;
        let testzi = (t.d_ziorigin + self.fv * t.d_zistepv) as f64 + fu * t.d_zistepu as f64;
        if newzibottom >= testzi {
            return true;
        }
        let newzitop = newzi * 1.01;
        newzitop >= testzi && s.d_zistepu >= t.d_zistepu
    }

    /// R_LeadingEdge, its gotos as a state machine.
    fn leading_edge(&mut self, edge: usize) {
        if self.edges[edge].surfs[1] == 0 {
            return;
        }
        let surf = self.edges[edge].surfs[1];
        self.surfs[surf].spanstate += 1;
        if self.surfs[surf].spanstate != 1 {
            return;
        }
        if self.surfs[surf].insubmodel {
            self.r_bmodelactive += 1;
        }
        let mut surf2 = self.surfs[1].next;
        #[derive(PartialEq)]
        enum Go {
            NewTop,
            Search,
            GotPosition,
        }
        let mut go = Go::Search;
        // (id's two tests, each with its own `goto newtop`: kept apart.)
        #[allow(clippy::if_same_then_else)]
        if self.surfs[surf].key < self.surfs[surf2].key {
            go = Go::NewTop;
        } else if self.surfs[surf].insubmodel && self.surfs[surf].key == self.surfs[surf2].key && self.zi_in_front(surf, surf2, edge) {
            go = Go::NewTop;
        }
        let mut guard = 4 * self.surfs.len() + 8;
        while go == Go::Search {
            // continue_search:
            loop {
                surf2 = self.surfs[surf2].next;
                guard -= 1;
                assert!(guard > 0, "R_LeadingEdge's search does not end");
                if self.surfs[surf].key <= self.surfs[surf2].key {
                    break;
                }
            }
            if self.surfs[surf].key == self.surfs[surf2].key {
                if !self.surfs[surf].insubmodel {
                    continue;
                }
                if self.zi_in_front(surf, surf2, edge) {
                    go = Go::GotPosition;
                }
                continue;
            }
            go = Go::GotPosition;
        }
        if go == Go::NewTop {
            let iu = (self.edges[edge].u >> 20) as i32;
            if iu > self.surfs[surf2].last_u {
                let u = self.surfs[surf2].last_u;
                self.emit(surf2, u, iu - u);
            }
            self.surfs[surf].last_u = iu;
        }
        // gotposition: insert before surf2
        self.surfs[surf].next = surf2;
        self.surfs[surf].prev = self.surfs[surf2].prev;
        let p = self.surfs[surf2].prev;
        self.surfs[p].next = surf;
        self.surfs[surf2].prev = surf;
    }

    /// R_GenerateSpans.
    fn generate_spans(&mut self) {
        self.r_bmodelactive = 0;
        self.surfs[1].next = 1;
        self.surfs[1].prev = 1;
        self.surfs[1].last_u = self.edge_head_u_shift20;
        let mut edge = self.edges[C_HEAD].next;
        let mut guard = self.edges.len() + 2;
        while edge != C_TAIL {
            guard -= 1;
            assert!(guard > 0, "R_GenerateSpans does not end");
            let e = self.edges[edge];
            if e.surfs[0] != 0 && e.surfs[1] != 0 {
                self.reached.both_sides += 1;
            }
            if e.surfs[0] != 0 {
                self.trailing_edge(e.surfs[0], edge);
                if e.surfs[1] == 0 {
                    edge = self.edges[edge].next;
                    continue;
                }
            }
            self.leading_edge(edge);
            edge = self.edges[edge].next;
        }
        self.cleanup_span();
    }

    fn snap(&self) -> Snap {
        let mut active = Vec::new();
        let mut e = self.edges[C_HEAD].next;
        let mut guard = self.edges.len() + 2;
        while e != C_TAIL && e != CNULL && guard > 0 {
            active.push((e, self.edges[e].u));
            e = self.edges[e].next;
            guard -= 1;
        }
        let surfs = self.surfs.iter().map(|s| (s.next, s.prev, s.last_u, s.spanstate)).collect();
        (active, surfs)
    }

    /// The line's table (after insertion), each edge's place in it.
    fn mark_line(&mut self) {
        self.line_pos.iter_mut().for_each(|p| *p = usize::MAX);
        self.pushed_this_line.iter_mut().for_each(|p| *p = false);
        let mut e = self.edges[C_HEAD].next;
        let mut k = 1;
        let mut guard = self.edges.len() + 2;
        while e != C_TAIL && guard > 0 {
            self.line_pos[e] = k;
            k += 1;
            e = self.edges[e].next;
            guard -= 1;
        }
    }

    /// R_ScanEdges, for a view at (0, 0), `w x h`, with no span-pool flush (a
    /// flush draws and empties the surfaces' span lists; it reads and writes
    /// no edge and no surface the scan reads).
    fn scan_edges(&mut self) {
        let w = self.w as i64;
        self.edges[C_HEAD] = CEdge { u: 0, u_step: 0, prev: CNULL, next: C_TAIL, surfs: [0, 1], nextremove: CNULL };
        self.edge_head_u_shift20 = (self.edges[C_HEAD].u >> 20) as i32;
        self.edges[C_TAIL] = CEdge { u: (w << 20) + 0xFFFFF, u_step: 0, prev: C_HEAD, next: C_AFTERTAIL, surfs: [1, 0], nextremove: CNULL };
        self.edge_tail_u_shift20 = (self.edges[C_TAIL].u >> 20) as i32;
        self.edges[C_AFTERTAIL] = CEdge { u: -1, u_step: 0, prev: C_TAIL, next: C_SENTINEL, surfs: [0, 0], nextremove: CNULL };
        // 2000 << 24 in a 32-bit int
        self.edges[C_SENTINEL] =
            CEdge { u: i64::from(2000i32.wrapping_shl(24)), u_step: 0, prev: C_AFTERTAIL, next: CNULL, surfs: [0, 0], nextremove: CNULL };
        let bottom = self.h as i32 - 1;
        let mut iv = 0;
        while iv < bottom {
            self.current_iv = iv;
            self.fv = iv as f32;
            self.surfs[1].spanstate = 1;
            if self.newedges[iv as usize] != CNULL {
                let first = self.edges[C_HEAD].next;
                self.insert_new_edges(self.newedges[iv as usize], first);
                if self.ub {
                    return;
                }
            }
            self.mark_line();
            self.generate_spans();
            let mut removed_here = vec![false; self.edges.len()];
            let mut r = self.removeedges[iv as usize];
            while r != CNULL {
                removed_here[r] = true;
                r = self.edges[r].nextremove;
            }
            if self.removeedges[iv as usize] != CNULL {
                self.remove_edges(self.removeedges[iv as usize]);
            }
            if self.edges[C_HEAD].next != C_TAIL {
                let first = self.edges[C_HEAD].next;
                self.step_active_u(first, &removed_here);
                if self.ub {
                    return;
                }
            }
            self.snaps.push(self.snap());
            self.reached.lines += 1;
            iv += 1;
        }
        self.current_iv = iv;
        self.fv = iv as f32;
        self.surfs[1].spanstate = 1;
        if self.newedges[iv as usize] != CNULL {
            let first = self.edges[C_HEAD].next;
            self.insert_new_edges(self.newedges[iv as usize], first);
            if self.ub {
                return;
            }
        }
        self.generate_spans();
        self.snaps.push(self.snap());
        self.reached.lines += 1;
        self.reached.spans += self.emitted.len() as u64;
    }
}

// ---------------------------------------------------------------------------
// An edge table: what R_EmitEdge and R_RenderFace leave for R_ScanEdges.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct TEdge {
    u: i64,
    u_step: i64,
    surfs: [u32; 2],
    v: i32,
    last: i32,
    /// The next in `newedges[v]`'s list.
    next: u32,
}

#[derive(Clone, Debug)]
struct TSurf {
    key: i32,
    insubmodel: bool,
    zi: [f32; 3],
}

#[derive(Clone, Debug)]
struct Table {
    w: usize,
    h: usize,
    /// Surfaces from 2 on (0 the dummy, 1 the background).
    surfs: Vec<TSurf>,
    /// Edges from FIRST_EDGE on, in emission order.
    edges: Vec<TEdge>,
    newedges: Vec<u32>,
}

impl Table {
    /// `R_EmitEdge`'s sort of edge `e` into `newedges[v]`.
    fn sort_in(&mut self, k: usize) {
        let e = FIRST_EDGE + k as u32;
        let ed = &self.edges[k];
        let mut u_check = ed.u;
        if ed.surfs[0] != 0 {
            u_check += 1;
        }
        let v = ed.v as usize;
        fn at(t: &Table, i: u32) -> &TEdge {
            &t.edges[(i - FIRST_EDGE) as usize]
        }
        let head = self.newedges[v];
        if head == NONE || at(self, head).u >= u_check {
            self.edges[k].next = head;
            self.newedges[v] = e;
        } else {
            let mut pcheck = head;
            loop {
                let nx = at(self, pcheck).next;
                if nx == NONE || at(self, nx).u >= u_check {
                    break;
                }
                pcheck = nx;
            }
            self.edges[k].next = at(self, pcheck).next;
            self.edges[(pcheck - FIRST_EDGE) as usize].next = e;
        }
    }

    fn port(&self) -> EdgeState {
        let mut s = EdgeState::new();
        s.w = self.w;
        s.h = self.h;
        s.edges = vec![Edge::ZERO; FIRST_EDGE as usize];
        for e in &self.edges {
            s.edges.push(Edge { u: e.u, u_step: e.u_step, prev: NONE, next: e.next, surfs: e.surfs, last: e.last, nearzi: 0.0, owner: NONE });
        }
        s.surfs = vec![Surf::ZERO, Surf { flags: SURF_DRAWBACKGROUND, key: 0x7FFF_FFFF, ..Surf::ZERO }];
        for t in &self.surfs {
            s.surfs.push(Surf { key: t.key, insubmodel: t.insubmodel, d_ziorigin: t.zi[0], d_zistepu: t.zi[1], d_zistepv: t.zi[2], ..Surf::ZERO });
        }
        s.newedges = self.newedges.clone();
        s.spans.clear();
        s.row_spans.clear();
        s
    }

    fn ids(&self) -> CScan {
        let mut edges = vec![CEdge { u: 0, u_step: 0, prev: CNULL, next: CNULL, surfs: [0, 0], nextremove: CNULL }; FIRST_EDGE as usize];
        let mut removeedges = vec![CNULL; self.h];
        for (k, e) in self.edges.iter().enumerate() {
            let next = if e.next == NONE { CNULL } else { e.next as usize };
            let idx = FIRST_EDGE as usize + k;
            // R_EmitEdge: edge->nextremove = removeedges[v2]; removeedges[v2] = edge;
            edges.push(CEdge { u: e.u, u_step: e.u_step, prev: CNULL, next, surfs: [e.surfs[0] as usize, e.surfs[1] as usize], nextremove: removeedges[e.last as usize] });
            removeedges[e.last as usize] = idx;
        }
        let blank = CSurf { next: 0, prev: 0, spans: CNULL, key: 0, last_u: 0, spanstate: 0, insubmodel: false, d_ziorigin: 0.0, d_zistepu: 0.0, d_zistepv: 0.0 };
        let mut surfs = vec![blank, CSurf { key: 0x7FFF_FFFF, ..blank }];
        for t in &self.surfs {
            surfs.push(CSurf { key: t.key, insubmodel: t.insubmodel, d_ziorigin: t.zi[0], d_zistepu: t.zi[1], d_zistepv: t.zi[2], ..blank });
        }
        let n = edges.len();
        CScan {
            w: self.w,
            h: self.h,
            edges,
            surfs,
            spans: Vec::new(),
            emitted: Vec::new(),
            newedges: self.newedges.iter().map(|&e| if e == NONE { CNULL } else { e as usize }).collect(),
            removeedges,
            current_iv: 0,
            fv: 0.0,
            edge_head_u_shift20: 0,
            edge_tail_u_shift20: 0,
            r_bmodelactive: 0,
            reached: Reached { tables: 1, ..Reached::default() },
            ub: false,
            snaps: Vec::new(),
            line_pos: vec![usize::MAX; n],
            pushed_this_line: vec![false; n],
        }
    }
}

/// The port's fused scan, line by line as `scan_edges` runs it (the snapshot
/// taken where id's loop ends a line), and the spans as (v, u, count, surf).
fn fused(t: &Table) -> (Spans, Vec<Snap>, Vec<u32>) {
    let mut s = t.port();
    let snap = |s: &EdgeState| -> Snap {
        let mut active = Vec::new();
        let mut e = s.edges[EDGE_HEAD as usize].next;
        let mut guard = s.edges.len() + 2;
        while e != EDGE_TAIL && e != NONE && guard > 0 {
            active.push((e as usize, s.edges[e as usize].u));
            e = s.edges[e as usize].next;
            guard -= 1;
        }
        let surfs = s.surfs.iter().map(|x| (x.next as usize, x.prev as usize, x.last_u, x.spanstate)).collect();
        (active, surfs)
    };
    let mut snaps = Vec::new();
    s.begin_scan();
    let bottom = s.h as i32 - 1;
    for iv in 0..bottom {
        s.scan_line(iv, true);
        snaps.push(snap(&s));
    }
    s.scan_line(bottom, false);
    snaps.push(snap(&s));
    s.row_spans.push(s.spans.len() as u32);
    // The function itself gives what this loop gave.
    let mut whole = t.port();
    whole.scan_edges();
    assert!(whole.row_spans == s.row_spans, "scan_edges is the loop");
    assert!(whole.spans.iter().zip(&s.spans).all(|(a, b)| (a.u, a.count, a.surf) == (b.u, b.count, b.surf)) && whole.spans.len() == s.spans.len());
    let mut out = Vec::new();
    for v in 0..s.h {
        for sp in &s.spans[s.row_spans[v] as usize..s.row_spans[v + 1] as usize] {
            out.push((v as i32, sp.u, sp.count, sp.surf as usize));
        }
    }
    (out, snaps, s.row_spans)
}

/// The port's own three-walk scan, `scan_edges_in_ids_walks`: spans and the
/// final table.
fn three_walk_reference(t: &Table) -> Reference {
    let mut s = t.port();
    s.scan_edges_in_ids_walks();
    let spans = s.spans.iter().map(|sp| (sp.u, sp.count, sp.surf as i32)).collect();
    let mut active = Vec::new();
    let mut e = s.edges[EDGE_HEAD as usize].next;
    let mut guard = s.edges.len() + 2;
    while e != EDGE_TAIL && e != NONE && guard > 0 {
        active.push((e as usize, s.edges[e as usize].u));
        e = s.edges[e as usize].next;
        guard -= 1;
    }
    (spans, active)
}

/// The outcome of one table.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Verdict {
    /// The fused scan is id's, line by line, and the port's three-walk scan agrees.
    Same,
    /// id's C has undefined behaviour (walks past edge_head); the fused scan
    /// and the port's three-walk scan agree.
    IdUndefined,
    /// They differ.
    Differs { line: usize, what: &'static str, reference_agrees: bool },
}

fn compare(t: &Table) -> (Verdict, Reached) {
    let (spans, snaps, _) = fused(t);
    let mut c = t.ids();
    c.scan_edges();
    c.reached.removed_on_first_line = t.edges.iter().filter(|e| e.last == e.v).count() as u64;
    let (rspans, ractive) = three_walk_reference(t);
    let fspans: Vec<(i32, i32, i32)> = spans.iter().map(|&(_, u, n, s)| (u, n, s as i32)).collect();
    let reference_agrees = rspans == fspans && snaps.last().map(|s| &s.0) == Some(&ractive);
    if c.ub {
        return (if reference_agrees { Verdict::IdUndefined } else { Verdict::Differs { line: 0, what: "id UB, reference too", reference_agrees } }, c.reached);
    }
    for (line, (a, b)) in snaps.iter().zip(&c.snaps).enumerate() {
        if a.0 != b.0 {
            return (Verdict::Differs { line, what: "table", reference_agrees }, c.reached);
        }
        if a.1 != b.1 {
            return (Verdict::Differs { line, what: "surfaces", reference_agrees }, c.reached);
        }
    }
    if snaps.len() != c.snaps.len() {
        return (Verdict::Differs { line: snaps.len().min(c.snaps.len()), what: "lines", reference_agrees }, c.reached);
    }
    if spans != c.emitted {
        let line = spans.iter().zip(&c.emitted).position(|(a, b)| a != b).unwrap_or(0);
        return (Verdict::Differs { line, what: "spans", reference_agrees }, c.reached);
    }
    // Each surface's span list, id's LIFO `pnext` chain, is the emission order
    // reversed (what D_DrawSurfaces walks): implied by the above, checked.
    for (si, s) in c.surfs.iter().enumerate() {
        let mut chain = Vec::new();
        let mut sp = s.spans;
        while sp != CNULL {
            chain.push((c.spans[sp].v, c.spans[sp].u, c.spans[sp].count));
            sp = c.spans[sp].pnext;
        }
        let mut mine: Vec<_> = spans.iter().filter(|x| x.3 == si).map(|&(v, u, n, _)| (v, u, n)).collect();
        mine.reverse();
        if chain != mine {
            return (Verdict::Differs { line: 0, what: "a surface's span list", reference_agrees }, c.reached);
        }
    }
    if !reference_agrees {
        return (Verdict::Differs { line: 0, what: "the port's three-walk scan", reference_agrees }, c.reached);
    }
    (Verdict::Same, c.reached)
}

// ---------------------------------------------------------------------------
// Random tables.
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    fn range(&mut self, a: i64, b: i64) -> i64 {
        if b <= a { a } else { a + self.below((b - a + 1) as u64) as i64 }
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

/// Where the edges may lie: `Clamped`, every `u` an edge has on every line it is
/// active within `R_EmitEdge`'s clamps (`vrect_x_adj_shift20 ..=
/// vrectright_adj_shift20`); `Screen`, within `edge_head.u ..= edge_tail.u`;
/// `Wild`, anywhere near the screen, past both sentinels.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Domain {
    Clamped,
    Screen,
    Wild,
}

fn random_table(rng: &mut Rng, domain: Domain) -> Table {
    let w = rng.range(1, 40) as usize;
    let h = rng.range(1, 24) as usize;
    let nsurf = rng.range(1, 10) as usize;
    let keys = rng.range(1, 6);
    let mut surfs = Vec::new();
    for _ in 0..nsurf {
        let mut zi = [rng.range(1, 1000) as f32 * 1e-4, rng.range(-50, 50) as f32 * 1e-6, rng.range(-50, 50) as f32 * 1e-6];
        if !surfs.is_empty() && rng.chance(25) {
            let other: &TSurf = &surfs[rng.below(surfs.len() as u64) as usize];
            zi = other.zi; // the same plane: the 0.99/1.01 test's tie
        }
        surfs.push(TSurf { key: rng.range(0, keys) as i32, insubmodel: rng.chance(35), zi });
    }
    let w64 = w as i64;
    let (lo, hi) = match domain {
        Domain::Clamped => ((1 << 19) - 1, (w64 << 20) + (1 << 19) - 1),
        Domain::Screen => (0, (w64 << 20) + 0xFFFFF),
        Domain::Wild => (-(3 << 20), ((w64 + 3) << 20)),
    };
    let snap = |rng: &mut Rng, u: i64| -> i64 {
        // Pile edges up on a few columns now and then: ties.
        match rng.below(4) {
            0 => (u >> 20 << 20) + 0xFFFFF,
            1 => (u >> 19) << 19,
            _ => u,
        }
    };
    let nedges = rng.range(0, 48) as usize;
    let mut t = Table { w, h, surfs, edges: Vec::new(), newedges: vec![NONE; h] };
    for k in 0..nedges {
        let v = rng.range(0, h as i64 - 1);
        let last = if rng.chance(30) { v } else { rng.range(v, h as i64 - 1) };
        let n = last - v;
        let (mut u, mut u_step);
        if domain == Domain::Wild {
            let r0 = rng.range(lo, hi);
            u = snap(rng, r0);
            let r1 = rng.range(-(w64 << 20), w64 << 20);
            u_step = if rng.chance(50) { rng.range(-(4 << 20), 4 << 20) } else { snap(rng, r1) };
        } else {
            // A start and an end inside the domain, and a step between them
            // (any step keeping every line's u inside).
            let r0 = rng.range(lo, hi);
            u = snap(rng, r0).clamp(lo, hi);
            let r1 = rng.range(lo, hi);
            let end = snap(rng, r1).clamp(lo, hi);
            u_step = if n > 0 { (end - u) / n } else { rng.range(-(w64 << 20), w64 << 20) };
            if n > 0 && rng.chance(30) {
                // any other step that stays inside (division truncates toward
                // zero: the ceiling of the negative bound, the floor of the other)
                u_step = rng.range((lo - u) / n, (hi - u) / n);
            }
            if rng.chance(15) {
                u_step = u_step >> 20 << 20; // whole pixels a line: more ties
                if (0..=n).any(|j| !(lo..=hi).contains(&(u + j * u_step))) {
                    u_step = 0;
                }
            }
            assert!((0..=n).all(|j| (lo..=hi).contains(&(u + j * u_step))), "the generator's own domain");
        }
        if domain != Domain::Wild {
            u = u.clamp(lo, hi);
        }
        let s1 = 2 + rng.below(nsurf as u64) as u32;
        let s2 = 2 + rng.below(nsurf as u64) as u32;
        let surfs = match rng.below(20) {
            0..=8 => [s1, 0],
            9..=17 => [0, s1],
            _ => [s1, s2],
        };
        t.edges.push(TEdge { u, u_step, surfs, v: v as i32, last: last as i32, next: NONE });
        t.sort_in(k);
    }
    t
}

fn report(name: &str, n: u64, r: &Reached, same: u64, undefined: u64, differ: u64) {
    eprintln!(
        "[{name}] tables {n} (same {same}, id-UB {undefined}, differ {differ}); lines {}, spans {}, inserted {}, removed {} (on their first line {}); \
         pushbacks {} (walked {}, to head {}, past a removed edge's place {}, next ends here {}, prev ends here {}, past one pushed this line {}); \
         ties {}; 1/z sorts {}; two-sided edges visits {}; tail pushed {}; walk past head {}",
        r.lines, r.spans, r.inserted, r.removed, r.removed_on_first_line, r.pushbacks, r.pushback_walked, r.pushback_to_head,
        r.pushback_past_removed, r.pushback_next_removed, r.pushback_prev_removed, r.pushback_past_pushed, r.ties, r.zi_sorts,
        r.both_sides, r.tail_pushed, r.head_ub
    );
}

#[test]
fn the_one_walk_scan_is_ids_on_random_tables() {
    let tables: u64 = std::env::var("QUAKE_SCAN_TABLES").ok().and_then(|s| s.parse().ok()).unwrap_or(20_000);
    for (domain, seed) in [(Domain::Clamped, 0x9E37_79B9_7F4A_7C15u64), (Domain::Screen, 0xD1B5_4A32_D192_ED03), (Domain::Wild, 0x2545_F491_4F6C_DD1D)] {
        let mut rng = Rng(seed);
        let mut total = Reached::default();
        let (mut same, mut undefined, mut differ) = (0, 0, 0);
        let mut first_diff: Option<(Table, Verdict)> = None;
        for _ in 0..tables {
            let t = random_table(&mut rng, domain);
            let (verdict, r) = compare(&t);
            total.add(&r);
            match verdict {
                Verdict::Same => same += 1,
                Verdict::IdUndefined => undefined += 1,
                Verdict::Differs { .. } => {
                    differ += 1;
                    if first_diff.as_ref().map_or(true, |(f, _)| t.edges.len() < f.edges.len()) {
                        first_diff = Some((t.clone(), verdict));
                    }
                }
            }
        }
        report(&format!("{domain:?}"), tables, &total, same, undefined, differ);
        if let Some((t, v)) = &first_diff {
            eprintln!("  smallest differing table: {v:?}\n  {t:?}");
        }
        if domain != Domain::Wild {
            assert_eq!(differ, 0, "{domain:?}: the fused scan differs from id's");
            assert_eq!(undefined, 0, "{domain:?}");
        }
    }
}

/// The smallest table on which the fused walk and id's differ: an edge that
/// steps right of `edge_tail` (which `R_EmitEdge`'s clamps never make).
#[test]
fn an_edge_past_the_tail_is_where_the_walks_part() {
    // 4 pixels wide, 3 lines: tail.u = (4 << 20) + 0xFFFFF. One leading edge
    // from line 0 to 2 at u = 3.5 px, stepping +2 px a line: on line 1 it is
    // at 5.5 px, past the tail.
    let w = 4usize;
    let mut t = Table { w, h: 3, surfs: vec![TSurf { key: 0, insubmodel: false, zi: [0.01, 0.0, 0.0] }], edges: Vec::new(), newedges: vec![NONE; 3] };
    t.edges.push(TEdge { u: (3 << 20) + (1 << 19), u_step: 2 << 20, surfs: [0, 2], v: 0, last: 2, next: NONE });
    t.sort_in(0);
    let (verdict, r) = compare(&t);
    eprintln!("[past the tail] {verdict:?}, tail pushed {}", r.tail_pushed);
    let (spans, snaps, _) = fused(&t);
    let mut c = t.ids();
    c.scan_edges();
    eprintln!("  fused spans {spans:?}\n  id's   spans {:?}\n  fused tables {:?}\n  id's   tables {:?}", c.emitted, snaps.iter().map(|s| &s.0).collect::<Vec<_>>(), c.snaps.iter().map(|s| &s.0).collect::<Vec<_>>());
    assert!(matches!(verdict, Verdict::Differs { .. }));
}

// ---------------------------------------------------------------------------
// Real maps' tables: what they reach.
// ---------------------------------------------------------------------------

/// The table `R_RenderWorld` and `R_DrawBEntitiesOnList` leave for the scan
/// of `scene` at `w x h` (the world and every inline model at rest).
fn real_table(scene: &Scene, w: usize, h: usize) -> Table {
    let bsp = scene.world;
    let mut edge = EdgeState::new();
    edge.begin_map(bsp);
    let frame = Frame::new(scene, w, h);
    edge.setup_frame(&frame);
    edge.mark_leaves(bsp);
    edge.begin_edge_frame();
    edge.render_world(bsp);
    let mut ents = vec![Ent { bsp, model: 0, origin: [0.0; 3], frame: 0, world_bsp: true, dlights: &[], rotation: world::IDENTITY_ROTATION }];
    for m in 1..bsp.models.len() {
        ents.push(Ent { bsp, model: m, origin: [0.0; 3], frame: 0, world_bsp: true, dlights: &[], rotation: world::IDENTITY_ROTATION });
    }
    edge.draw_bentities(bsp, &ents);
    let edges = edge.edges[FIRST_EDGE as usize..]
        .iter()
        .map(|e| TEdge { u: e.u, u_step: e.u_step, surfs: e.surfs, v: 0, last: e.last, next: e.next })
        .collect();
    let surfs = edge.surfs[2..].iter().map(|s| TSurf { key: s.key, insubmodel: s.insubmodel, zi: [s.d_ziorigin, s.d_zistepu, s.d_zistepv] }).collect();
    let mut t = Table { w, h, surfs, edges, newedges: edge.newedges.clone() };
    // each edge's first line, from the lists (the report's "first line" count)
    for v in 0..h {
        let mut e = t.newedges[v];
        while e != NONE {
            let k = (e - FIRST_EDGE) as usize;
            t.edges[k].v = v as i32;
            e = t.edges[k].next;
        }
    }
    t
}

#[test]
fn the_one_walk_scan_is_ids_on_the_maps() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../quake-data/ID1/PAK0.PAK");
    let Ok(pak) = crate::pak::Pak::open(&path) else {
        eprintln!("no pak");
        return;
    };
    let pal = [[0u8; 3]; 256];
    let views: u64 = std::env::var("QUAKE_SCAN_VIEWS").ok().and_then(|s| s.parse().ok()).unwrap_or(40);
    let mut rng = Rng(0x0123_4567_89AB_CDEF);
    let mut total = Reached::default();
    let (mut same, mut undefined, mut differ) = (0u64, 0u64, 0u64);
    for map in ["start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8"] {
        let world = Bsp::parse(&pak.read_file(&format!("maps/{map}.bsp")).expect("read").expect("the map")).expect("a bsp");
        // Eyes: the centres of random leaves that are not solid.
        let open: Vec<&crate::bsp::DLeaf> = world.leafs.iter().filter(|l| l.contents != CONTENTS_SOLID && l.maxs[0] > l.mins[0]).collect();
        for k in 0..views {
            let leaf = open[rng.below(open.len() as u64) as usize];
            let pos = [0, 1, 2].map(|i| (leaf.mins[i] as f32 + leaf.maxs[i] as f32) * 0.5);
            let cam = Camera {
                pos,
                yaw: rng.range(0, 359) as f32,
                pitch: rng.range(-70, 70) as f32,
                roll: if rng.chance(20) { rng.range(-80, 80) as f32 } else { 0.0 },
                fov_deg: [90.0, 90.0, 110.0, 60.0][(k % 4) as usize],
            };
            let (w, h) = [(320, 200), (701, 397), (1315, 535), (2631, 1071), (97, 1400)][rng.below(5) as usize];
            let scene = Scene::new(&world, cam, w, h, &pal);
            let t = real_table(&scene, w, h);
            // Every row's spans, in the order they are made, tile the row
            // once (what drawing them in row order rests on: no pixel in two spans).
            let (spans, _, _) = fused(&t);
            let mut at = (0i32, 0i32);
            for &(v, u, n, _) in &spans {
                if v != at.0 {
                    assert_eq!(at.1, w as i32, "{map} row {} ends short", at.0);
                    at = (v, 0);
                }
                assert!(u == at.1 && n > 0, "{map} {cam:?} {w}x{h}: row {v}: span at {u} after {}", at.1);
                at.1 = u + n;
            }
            assert_eq!(at, (h as i32 - 1, w as i32));
            let (verdict, r) = compare(&t);
            total.add(&r);
            match verdict {
                Verdict::Same => same += 1,
                Verdict::IdUndefined => undefined += 1,
                Verdict::Differs { .. } => {
                    differ += 1;
                    eprintln!("  {map} {cam:?} {w}x{h}: {verdict:?}");
                }
            }
        }
    }
    report("real maps", same + undefined + differ, &total, same, undefined, differ);
    assert_eq!(differ + undefined, 0);
}
