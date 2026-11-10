//! The settings of an engine.

/// The settings of an engine (see the spec in `lib.rs`).
#[derive(Clone, Debug)]
pub struct Params {
    pub threshold: f64,
    pub protect: i64,
    pub check: i64,
    pub ratio: f64,
    pub margin: f64,
    pub floor: f64,
    pub every: i64,
    pub least: f64,
    /// The working window (`target`, `cap`, `hard`, in tokens of the view without the system prompt; 0: off).
    /// `target`: the priced cap of `engine/cap.rs` runs with this budget (dead results leave when that is
    /// cheaper than keeping them, the cache included). `cap`: past it the oldest results the sticky set
    /// does not hold leave, price ignored. `hard`: past it the oldest sticky results leave too (as pointers).
    pub target: i64,
    pub cap: i64,
    pub hard: i64,
    pub turn_head: bool,
    pub eoq: i64,
    pub ledger: i64,
    pub link: f64,
    pub cards: i64,
    pub birth: Vec<String>,
    /// Tokens of the agent's findings and plan kept in the anchor (0: off).
    pub notes: i64,
    /// Tokens the person's standing instructions take before the findings and the pinned files give
    /// way to them (0: no rules block). A rule is never dropped for its size: past this budget the other
    /// blocks shrink instead. Also on or off (0): the standing facts of `standing.rs`, which override a default
    /// and come back in the tail of the view when an action supposes the default.
    pub rules: i64,
    /// Tokens of the files the person named, as last read, and of the files the agent wrote, as last
    /// written, kept in the anchor (0: off).
    pub pin: i64,
    /// Agent steps without a `Finding:`, `Plan:` or `Rule:` line after which the last tool result asks
    /// for them, at most once per as many steps (0: never).
    pub nudge: i64,
    /// The host adds the curator's directives to the system prompt (0: it does not).
    pub directives: i64,
    /// The host offers the model the `recall` tool, which brings back a message the view left out as it
    /// was (0: it does not; the tool list is then the one without the curator).
    pub recall: i64,
    /// A run of at least this many lines of a tool output that an earlier output holds too, and that
    /// earlier output is whole in the view, goes as one line that points to it; the run comes back whole
    /// once that output leaves the view or is cut (0: off). With `asked`, a range the agent asks for again goes
    /// whole: it means to look at it closely, and a pointer in its place sends it looking for a way round.
    pub dedup: i64,
    /// Past the cap, results go by need before the oldest messages leave: first the parts a later
    /// output in the view holds line for line, then the parts least likely to be needed (false: the
    /// oldest messages leave as soon as what the model rules out is not enough).
    pub cut_need: bool,
    /// Past the cap, only the parts a later output in the view holds line for line go; the cut that loses
    /// material waits until the view is past twice the cap, and below that the usual rent rule runs.
    pub cut_copies: bool,
    /// Past the cap, the parts of results that tell the most per token stay and the rest go (`Engine::cover`)
    /// before the oldest messages leave.
    pub cut_cover: bool,
    /// A request the agent closed with a reply (its last message holds text and no call) keeps, once the
    /// next request begins, only that request and that reply: the calls made for it and their outputs leave
    /// the view, and one line on the request says which (0: off). The reply is what the agent took from
    /// them, the way a note is.
    pub close: i64,
    /// How much of the calls so far a cut counts on as still to come, the rent it saves (at least 4 calls,
    /// never past the point where the cap would take it for free).
    pub ahead: f64,
    /// Below the cap too, the parts of results whose every line a later output in the view sends are put to
    /// the rent rule with the parts the model rules out: whoever reads the view loses nothing by them, and
    /// they go once keeping them has cost more than the cache they break (0: only past the cap, `cut`).
    pub sup: i64,
    /// Tokens the other topics must hold together before they leave the view: leaving one out costs a
    /// step to bring it back when it was needed, and saves in proportion to its size (0: they always leave).
    pub apart: i64,
    /// With `close`, a step that read a file the latest request names stays, whatever else would let it go:
    /// the request is about that file (0: off).
    pub named: i64,
    /// With `named`, a step that read a file the thread's first message names stays until a later request
    /// of the thread names that file, once a later request took up another file of that list: the list is
    /// being worked through, and no request has yet been about this one (0: off).
    pub mission: i64,
    /// With `close`, the outputs of the request still open go idle when no later agent message draws on
    /// them (`live_drop`), and leave when that pays (0: off).
    pub live: i64,
    /// With `live`, an output that leaves keeps its leads: the lines with a name no other output holds,
    /// of a form the agent has written before (`loc_3D60` after `loc_2A10`, `user_42` after `user_7`): a
    /// reference not yet followed (0: off).
    pub lead: i64,
    /// Recall brings back, for a word, the steps whose calls went for it (`cat auth.rs` for `auth`) over
    /// those that only came across it (an `ls` that printed `auth.rs`), and takes those words first (0: off).
    pub aim: i64,
    /// With `live`, a name the agent's call went for keeps an older output in use only if the call's own
    /// output lacks it: once `grep loc_1EF8` has printed what `loc_1EF8` leads to, the listing that
    /// pointed there is not what the agent works from (0: off).
    pub follow: i64,
    /// With `live`, an output goes idle only once the agent has written about what it read since: text,
    /// or a note. Calls alone are the agent still looking; what it read is not yet digested (0: off).
    pub digest: i64,
    /// The size cap (`eoq`) leaves out of its count the outputs the agent has not written about yet (no text and
    /// no note after them): material the agent is still reading is not what makes a context costly to carry,
    /// and taking it away leaves the agent blind to it (0: off).
    pub hold: i64,
    /// Below the cap and past it alike, the latest outputs the agent has not written about yet (no text and
    /// no note after them), up to this many tokens, leave only as an exact copy of a later output, never on a
    /// guess that they are not needed: the agent is still working from them, and a new request from the person
    /// does not change that. Older ones past the budget may go on a guess, so a session where the agent never
    /// writes still has a bounded view (0: a guess may take any).
    pub still: i64,
    /// With `dedup`, an output that is mostly lines already in the view goes whole: the agent asked for it again,
    /// as it did not find or trust the copy above (0: such an output goes as pointers too; 2: only when most of its
    /// lines went as a pointer once already, so the first copy asked again still goes as one; 3: as 1, but an
    /// output an earlier call gave, line for line, goes as one line that says so, and that nothing changed when
    /// it was the same call).
    pub asked: i64,
    /// With `dedup`, the output that has just arrived goes whole, as it came: the agent asked for it to look at
    /// it now, and a pointer in its place sends it looking for a way round (it asks again, reads it by other
    /// means, or distrusts the tool). Its runs go as pointers from the next request on, which rewrites only that
    /// one message, at the end of what the cache holds (0: they go as pointers as the output arrives).
    pub fresh: i64,
    /// The note that takes the place of an output left out says only that it left and how to bring it back:
    /// in a long open request every such note is sent again on each step, so its words add up (0: off).
    pub brief: i64,
    /// A read of a file that a later edit or write of it (one that did not fail) has changed is out of date:
    /// past the cap it leaves with the parts a later output holds line for line, before any part that would
    /// leave on a guess, and the note left in its place names the file and the message of the edit. Decided
    /// only when the view is cut, so the view between two cuts stays as it was sent (0: off).
    pub stale: i64,
    /// The provider keeps more than the last request in its cache: the last view sent on each topic (a
    /// pocket) lives `life` seconds. A request back on a topic whose pocket lives reads it from the cache, so
    /// the switch costs nothing. How long the provider keeps a view is also learned from what it says it read
    /// from its cache (`Curator::cached`). 2: also, a view whose pocket is gone is read again anyway, so it is
    /// cut down then, and a cut past where the cache stops is priced as free: cheaper after long pauses, but
    /// what is cut early is missed more (0: off, the cache holds the last view).
    pub pockets: i64,
    /// With a `target`, the view is rewritten only at an epoch: once keeping its growth costs more than the
    /// rewrite (`Engine::epoch_due`) or it reaches the ceiling. Between epochs it only grows at the end, so the
    /// provider's cache holds all of it but the last request; what the request needs of the history comes in the tail
    /// of the view (the index), not by changing messages before it. At an epoch everything that waited
    /// happens at once: the other requests close, the dead outputs leave, the window is cut (0: each of
    /// those happens when it pays by itself, as before).
    pub epoch: i64,
    /// Seconds a pocket is taken to live at most (`pockets`); 0: only the last view sent, as a local server
    /// with one slot.
    pub life: f64,
    /// Not 0: at an epoch the window cut and the closes land only when they pay for what they send again.
    /// What leaves saves `ratio` of its tokens on each call still to come (`lcalls` and `lahead`); what stays
    /// behind the first message changed is read once more without the cache. 0: the window cut runs at
    /// every epoch whatever it sends again, and a close behind it is taken as free.
    /// 2: the closes are also priced together with a cut that does not pay alone.
    /// 3: nothing is priced ahead. An epoch that is due is made whole on a copy of the engine, and it lands only
    /// when the view it really makes pays for what it really sends again (`Engine::landing_pays`): else the view
    /// goes on growing at the end, as if the epoch had not been tried, and the next try waits for `check` tokens
    /// of growth. The first view, a view past the ceiling and a fixed `cap` land whatever they cost.
    /// 4: also, an epoch is tried each `check` tokens of growth past the fire point, without waiting for the
    /// room past the last landing: a landing that ended large is followed by the next one that pays.
    /// Measured 2026-11-10 on the 484 recorded sessions, cold replay at a 10 % cache price, against 0. With 3:
    /// input cost 0.67 % lower (1.2 % in sessions of 26 to 100 requests, 3.1 % in longer ones), 216 forgotten
    /// words against 223 (fixtures 36 against 39), 6 sessions dearer than plain history against 11, views of the
    /// same mean size; the 33-request session that showed the defect costs 0.0844 $ against 0.0984 $ (plain
    /// 0.0918 $). It is not the default because of one fixture: on realm100 it refuses four rewrites that take
    /// 0.2k to 5.5k tokens out and send 35k to 42k again, and the view reaches 52455 tokens before the rewrite
    /// that pays, past the 50000 `the_window_holds_on_every_fixture` allows. With 4: 0.99 % lower and that session
    /// 0.0804 $, but 242 forgotten words (fixtures 47): the earlier landings cut more.
    pub kept: i64,
    /// With `kept`: the calls a session still has, whatever its length (the recorded sessions have about 12).
    pub lcalls: f64,
    /// With `kept`: the calls still to come that each call already made adds (about 0.2 on the recorded sessions).
    pub lahead: f64,
    /// With `kept`: how many times its price a landing has to save before it is made.
    pub lmargin: f64,
}

impl Params {
    pub(super) fn stage(&self, s: &str) -> bool {
        self.birth.iter().any(|b| b == s)
    }
}
