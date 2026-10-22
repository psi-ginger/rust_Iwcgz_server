//! Typed id domains and the pools that mint them — Phase P1 of
//! `docs/DESIGN_THREE_TIER_ID_ARCHITECTURE.md` §3.
//!
//! > **`P1`…`P4` are not a schedule — see D9.**
//! > The design doc planned a *progressive* widening: P2 raises [`T2_TOP`], P3
//! > opens Tier 3 behind a level floor, P4 widens the SI16 frame fields to UI16.
//! > **`docs/rust-rewrite/ROADMAP.md` §D9 supersedes that plan**: when the
//! > client/wire expansion actually happens it is **one-shot to `u32`, in a
//! > single pass** — no intermediate constant rewrite, no staged gates. Every
//! > `P1`…`P4` mentioned below is only the *name of a switch or refusal that
//! > exists today*; it must not be read as work to schedule or execute.
//!
//! Three domains with no conversion between them, so cross-band assignment is a
//! compile error rather than a runtime surprise (§5.0, Cat spec 3):
//!
//! - [`PlayerCharUnique`] — session ids `1..=8_499` ([`PLAYER_BAND_TOP`]).
//!   Minted only by the batch-2 wrap in `main.rs` (T6); the constructor here
//!   validates the band at the boundary.
//! - [`StandardMonsterId`] — field monsters on today's live walls,
//!   `8_500..=19_999` (audit H3's band). One pool, [`DualTierPool`], holding
//!   the §3.5 mutual borrow (T1 ↔ T2) inside the band.
//! - [`Tier3MonsterId`] — the extended endgame band `30_000..=59_999` in an
//!   isolated [`WindowPool`] nothing else can reach. Its *issuance* stays off
//!   until the client rebuild opens it (Gate 0: the shipped client's tables
//!   stop at `MAX_CHAR_NUMBER = 20_000`, design doc §2.3; that widening is
//!   D9's one-shot `u32` change) — `tier3_level_floor` is `None` — but the
//!   pool exists so its shape is testable today (T9/T11).
//!
//! [`MonsterEntityId`] is the storage enum both monster types funnel through,
//! so one `Vec<Monster>` holds either without a bare int at a call site (§5.0).
//! Wire conversions live on the types: `.si16()` / `.u16()` at encode sites,
//! [`PlayerCharUnique::try_from_si16`] at parse sites.

use std::fmt;
use std::num::NonZeroU16;
use std::sync::Mutex;

use gz_data::npc::{NPC_CHAR_UNIQUE_BASE, NPC_PLACEMENTS};

use crate::logln_err;

/// Exclusive top of the *client's* char tables, from the Gate 0 disassembly
/// sweep of the shipped `New Era.exe` (design doc §2.3: 239 dword table guards
/// `cmp …, 0x4E20` and 54 `IsNPC` pairs all agree): `MAX_CHAR_NUMBER = 20000`.
/// Every char-unique the shipped client can index a table with is `<` this.
pub(crate) const CLIENT_TABLE_TOP: u16 = 20_000;

/// Top of the player id band, tied to the NPC band's floor so the two can never
/// overlap (audit H3). The registry's `PLAYER_CHAR_UNIQUE_TOP` aliases this.
pub(crate) const PLAYER_BAND_TOP: i16 = NPC_CHAR_UNIQUE_BASE as i16 - 1;

/// Tier 1's top — the legacy band's upper half. [`DualTierPool`] borrows into
/// Tier 2 when a spawn asks for a Tier-1 id and T1 is spent (§3.5).
pub(crate) const T1_TOP: u16 = 18_999;

/// Tier 2's floor: one above [`T1_TOP`], so the two tiers partition the
/// standard band and a borrowed id stays recognisable by range alone.
pub(crate) const T2_FLOOR: u16 = T1_TOP + 1;

/// Top of Tier 2 — today's true live wall: Gate 0 measured the shipped client's
/// `MAX_CHAR_NUMBER = 20_000` (design doc §2.3), so no id ≥ this is visible to
/// a stock client. The design doc's "raise this to `29_999` after the rebuild"
/// is **not** a scheduled stage any more: D9 collapses the staged plan into one
/// change (`docs/rust-rewrite/ROADMAP.md`), which this constant follows rather
/// than leads (§4).
pub(crate) const T2_TOP: u16 = CLIENT_TABLE_TOP - 1;

/// Tier 3's floor — the legacy partition's extended band
/// (`Char-Server.h:1006-1037`), client-gated (§6) until the rebuild D9
/// specifies as a one-shot `u32` widening.
pub(crate) const T3_FLOOR: u16 = 30_000;

/// Tier 3's top: `2 × MAX_CHAR_NUMBER − 1`, the second client table the
/// rebuild widens to (§2.4).
pub(crate) const T3_TOP: u16 = 59_999;

/// Largest id an SI16 frame field can carry (`siCharUnique` and friends). A
/// Tier-3 id above this has no interactive frame. Lifting it is D9's single
/// one-shot move to `u32` when the wire changes — explicitly **not** an
/// intermediate `SI16 → UI16` stage (§4); [`Tier3MonsterId::try_si16`] is
/// where that refusal lives.
pub(crate) const WIRE_SI16_TOP: u16 = 32_767;

/// Why an id was refused. Displayed in the log lines that accompany the `None`
/// an allocation or parse returns — never a panic (A8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IdError(i32);

impl IdError {
    const fn new(raw: i32) -> Self {
        Self(raw)
    }
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "id {} is outside its band", self.0)
    }
}

impl std::error::Error for IdError {}

/// A session's id — `siCharUnique`, the band `1..=[PLAYER_BAND_TOP]` (design
/// doc §3.4). Minted only by the batch-2 wrap in `main.rs` (T6) and by capture
/// mode; every construction validates the band, which is what makes
/// `Registry::register`'s own check belt-and-braces rather than load-bearing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlayerCharUnique(NonZeroU16);

impl PlayerCharUnique {
    /// The wire value: the invariant (`≤ 8_499`) makes the cast lossless.
    #[must_use]
    pub(crate) fn si16(self) -> i16 {
        self.0.get() as i16
    }

    /// Parses a wire `siCharUnique` into the type, refusing `≤ 0` (the legacy's
    /// `GetIDFromCharUnique` returns 0 for those, `CharManager.cpp:205`) and
    /// anything above the player band — which the client would classify as an
    /// instance NPC or an NPC, not a person (Gate 0b, design doc §2.3).
    ///
    /// # Errors
    ///
    /// [`IdError`] carrying the refused raw value.
    pub(crate) fn try_from_si16(raw: i16) -> Result<Self, IdError> {
        if (1..=PLAYER_BAND_TOP).contains(&raw)
            && let Some(n) = NonZeroU16::new(raw as u16)
        {
            Ok(Self(n))
        } else {
            Err(IdError::new(i32::from(raw)))
        }
    }
}

impl fmt::Display for PlayerCharUnique {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.si16())
    }
}

/// Which half of the standard band a [`StandardMonsterId`] sits in — the §3.5
/// free-list origin, so a recycled id returns to the pool that minted it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tier {
    Tier1,
    Tier2,
}

/// A field monster's id on today's live walls: the audit-H3 band
/// `8_500..=[T2_TOP]` (design doc §3.4). Never a player id — the floors are
/// disjoint by construction (invariant 2, §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StandardMonsterId(NonZeroU16);

impl StandardMonsterId {
    /// The wire value, lossless at any `T2_TOP` the design allows (`19_999`
    /// today, `29_999` if the wall is ever raised — either way `< 32_767`).
    #[must_use]
    pub(crate) fn si16(self) -> i16 {
        self.0.get() as i16
    }

    #[must_use]
    pub(crate) fn u16(self) -> u16 {
        self.0.get()
    }

    /// The band half this id came from — [`DualTierPool`]'s free routing.
    #[must_use]
    pub(crate) fn tier(self) -> Tier {
        if self.u16() >= T2_FLOOR {
            Tier::Tier2
        } else {
            Tier::Tier1
        }
    }

    /// Parses a raw value into the standard band, refusing the player band
    /// below (H3) and anything above the live wall.
    ///
    /// # Errors
    ///
    /// [`IdError`] carrying the refused raw value.
    pub(crate) fn try_from_u16(raw: u16) -> Result<Self, IdError> {
        if (NPC_CHAR_UNIQUE_BASE..=T2_TOP).contains(&raw)
            && let Some(n) = NonZeroU16::new(raw)
        {
            Ok(Self(n))
        } else {
            Err(IdError::new(i32::from(raw)))
        }
    }
}

impl fmt::Display for StandardMonsterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.u16())
    }
}

/// A Tier-3 id: the extended band `30_000..=[T3_TOP]` (design doc §3.4). The
/// pool is isolated by construction — [`WindowPool`]'s own floor refuses a
/// standard-band free, so the two domains cannot leak into each other even
/// through corruption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tier3MonsterId(NonZeroU16);

impl Tier3MonsterId {
    #[must_use]
    pub(crate) fn u16(self) -> u16 {
        self.0.get()
    }

    /// The SI16 frame conversion, refusing ids above [`WIRE_SI16_TOP`] — the
    /// wire gate in one method (design doc §5.0, invariant 7). No pool mints
    /// an id above the cap, so `Some` is the only result today; D9 makes
    /// lifting it a single one-shot `u32` widening rather than a `UI16` stage
    /// (`docs/rust-rewrite/ROADMAP.md`).
    #[must_use]
    pub(crate) fn try_si16(self) -> Option<i16> {
        if self.u16() <= WIRE_SI16_TOP {
            Some(self.u16() as i16)
        } else {
            None
        }
    }

    /// Parses a raw value into the Tier-3 band.
    ///
    /// # Errors
    ///
    /// [`IdError`] carrying the refused raw value.
    pub(crate) fn try_from_u16(raw: u16) -> Result<Self, IdError> {
        if (T3_FLOOR..=T3_TOP).contains(&raw)
            && let Some(n) = NonZeroU16::new(raw)
        {
            Ok(Self(n))
        } else {
            Err(IdError::new(i32::from(raw)))
        }
    }
}

impl fmt::Display for Tier3MonsterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.u16())
    }
}

/// Storage for one live monster: one enum so the world's `Vec<Monster>` holds
/// either strong type without a bare int ever existing at a call site (design
/// doc §5.0). Variants wrap the strong types and nothing else can construct
/// them, so a player id is unreachable here by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MonsterEntityId {
    Standard(StandardMonsterId),
    Tier3(Tier3MonsterId),
}

impl MonsterEntityId {
    /// The UI16 / SI32 wire value (spawn keys, 241 order ids, logout 11).
    #[must_use]
    pub(crate) fn u16(self) -> u16 {
        match self {
            Self::Standard(id) => id.u16(),
            Self::Tier3(id) => id.u16(),
        }
    }

    /// The SI16 frame conversion: infallible for standard ids, and for Tier 3
    /// refused above [`WIRE_SI16_TOP`] (see [`Tier3MonsterId::try_si16`]; D9
    /// is what would lift it, in one pass). Encode sites that need an SI16
    /// frame skip it on `None` — a frame that cannot name its subject must not
    /// be invented (design doc §4).
    #[must_use]
    pub(crate) fn si16(self) -> Option<i16> {
        match self {
            Self::Standard(id) => Some(id.si16()),
            Self::Tier3(id) => id.try_si16(),
        }
    }
}

impl fmt::Display for MonsterEntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.u16())
    }
}

/// One tier's window of ids: a free list plus a monotonic head, the legacy's
/// `FindEmptyCharUnique` split into its two real halves (`Char-Server.h:
/// 1006-1037`). `free` before head, so a recycled id goes straight back out
/// (T10); both checked against `[floor, top]` on the way in *and* out — an id
/// from another band can never be minted here even if a corrupted free list
/// holds one (H3).
#[derive(Debug)]
struct WindowPool {
    floor: u16,
    top: u16,
    free: Vec<u16>,
    /// Next never-used id; one past `top` means spent.
    head: u16,
}

impl WindowPool {
    const fn new(floor: u16, top: u16) -> Self {
        Self {
            floor,
            top,
            free: Vec::new(),
            head: floor,
        }
    }

    /// Takes one id — free list first, then the head, then `None` when both
    /// halves are spent (A8: refusal, never a panic).
    fn take(&mut self) -> Option<u16> {
        while let Some(raw) = self.free.pop() {
            if (self.floor..=self.top).contains(&raw) {
                return Some(raw);
            }
            // Defensive: nothing in this world frees a foreign-band id, so a
            // hit here is corruption — discard rather than mint it (H3).
            logln_err!("ids: discarding out-of-band freed id {raw}");
        }
        if self.head > self.top {
            return None;
        }
        let fresh = self.head;
        self.head += 1;
        Some(fresh)
    }

    /// Returns one id to this pool — the origin pool, by range (T10). A foreign
    /// id is refused here too: the typed free path cannot produce one, so this
    /// is corruption again.
    fn give(&mut self, raw: u16) {
        if (self.floor..=self.top).contains(&raw) {
            self.free.push(raw);
        } else {
            logln_err!("ids: refusing to free out-of-band id {raw} into {:?}", self);
        }
    }
}

/// The standard band's two tiers and the §3.5 mutual borrow (Cat deliverable
/// 2): a T1 request spends T1's free list, then T1's head, then **borrows T2's
/// free list and head** — the T1→T2 direction; `take2` is the symmetric
/// T2→T1. Borrowed ids stay `StandardMonsterId` and are freed back to the
/// *origin* pool by range ([`StandardMonsterId::tier`]), so the borrow costs
/// capacity only until the id comes home (T10).
#[derive(Debug)]
struct DualTierPool {
    t1: WindowPool,
    t2: WindowPool,
}

impl DualTierPool {
    fn new() -> Self {
        Self {
            t1: WindowPool::new(NPC_CHAR_UNIQUE_BASE + NPC_PLACEMENTS.len() as u16, T1_TOP),
            t2: WindowPool::new(T2_FLOOR, T2_TOP),
        }
    }

    /// T1 first, borrowing T2 when T1 is spent.
    fn take1(&mut self) -> Option<u16> {
        self.t1.take().or_else(|| self.t2.take())
    }

    /// T2 first, borrowing T1 when T2 is spent.
    fn take2(&mut self) -> Option<u16> {
        self.t2.take().or_else(|| self.t1.take())
    }
}

/// The monster-side allocator (design doc §3.5): one [`DualTierPool`] for the
/// standard band and one isolated [`WindowPool`] for Tier 3, each behind its
/// own lock. There is deliberately **no player pool** — player ids stay the
/// batch-2 wrap's job in `main.rs` (T6), so isolation is by absence: every id
/// this type can mint is ≥ `8_500` (invariant 2, §7.1).
#[derive(Debug)]
pub(crate) struct IdAllocator {
    standard: Mutex<DualTierPool>,
    tier3: Mutex<WindowPool>,
    /// The Tier-3 level floor: `None` (the shipped default) keeps every spawn
    /// on the standard pools regardless of level; `Some(floor)` routes
    /// `level ≥ floor` to Tier 3 first via [`spawn_tier`]. There is no setter —
    /// the gate is the client rebuild, and D9 fixes *how* that rebuild widens
    /// the wire (one-shot `u32`), so this never opens as a staged "P3" step
    /// (§4, `docs/rust-rewrite/ROADMAP.md`).
    tier3_level_floor: Option<u16>,
}

impl Default for IdAllocator {
    fn default() -> Self {
        Self {
            standard: Mutex::new(DualTierPool::new()),
            tier3: Mutex::new(WindowPool::new(T3_FLOOR, T3_TOP)),
            tier3_level_floor: None,
        }
    }
}

impl IdAllocator {
    /// A fresh allocator: standard floor above the static-NPC block (so a
    /// hand-built world can never mint a static NPC's id, H3), Tier 3 closed.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The Tier-3 level floor, `None` until the client rebuild lands (§6;
    /// D9 specifies that rebuild's wire change as one-shot `u32`).
    #[must_use]
    pub(crate) fn tier3_level_floor(&self) -> Option<u16> {
        self.tier3_level_floor
    }

    /// A Tier-1 id — [`DualTierPool`]'s T1→T2 borrow included. `None` when
    /// both halves of the standard band are spent (A8).
    pub(crate) fn alloc_tier1(&self) -> Option<StandardMonsterId> {
        // A poisoned lock means a panic happened inside another allocator
        // call; refusing to mint beats sharing a suspect pool (A8).
        self.standard
            .lock()
            .ok()?
            .take1()
            .and_then(|raw| StandardMonsterId::try_from_u16(raw).ok())
    }

    /// A Tier-2 id — [`DualTierPool`]'s T2→T1 borrow included.
    pub(crate) fn alloc_tier2(&self) -> Option<StandardMonsterId> {
        self.standard
            .lock()
            .ok()?
            .take2()
            .and_then(|raw| StandardMonsterId::try_from_u16(raw).ok())
    }

    /// A Tier-3 id from the isolated pool. Never borrows (Tier 3 has no
    /// sibling, §3.5); returns `None` while the pool is un-issued (it is
    /// today, the floor being `None`) or spent.
    pub(crate) fn alloc_tier3(&self) -> Option<Tier3MonsterId> {
        self.tier3
            .lock()
            .ok()?
            .take()
            .and_then(|raw| Tier3MonsterId::try_from_u16(raw).ok())
    }

    /// Returns a dead monster's id to the pool that minted it — origin by
    /// range, so a borrowed T1→T2 id goes back to T2 (T10). The typed argument
    /// makes a player-band free impossible to express.
    pub(crate) fn free_monster(&self, id: MonsterEntityId) {
        match id {
            MonsterEntityId::Standard(s) => match self.standard.lock() {
                Ok(mut pool) => match s.tier() {
                    Tier::Tier1 => pool.t1.give(s.u16()),
                    Tier::Tier2 => pool.t2.give(s.u16()),
                },
                Err(_) => logln_err!("ids: standard pool lock poisoned; {s} not recycled"),
            },
            MonsterEntityId::Tier3(t) => match self.tier3.lock() {
                Ok(mut pool) => pool.give(t.u16()),
                Err(_) => logln_err!("ids: tier-3 pool lock poisoned; {t} not recycled"),
            },
        }
    }

    /// Both standard free lists, copied out — the T7/T10 test seam (which pool
    /// holds which id), test builds only.
    #[cfg(test)]
    pub(crate) fn free_snapshot(&self) -> (Vec<u16>, Vec<u16>, Vec<u16>) {
        let (t1, t2) = match self.standard.lock() {
            Ok(pool) => (pool.t1.free.clone(), pool.t2.free.clone()),
            Err(_) => (Vec::new(), Vec::new()),
        };
        let t3 = self
            .tier3
            .lock()
            .map(|p| p.free.clone())
            .unwrap_or_default();
        (t1, t2, t3)
    }

    /// Corrupts T1's free list with a raw id — the T7 discard-rule test. No
    /// production path can reach this (the typed `free_monster` validates),
    /// which is the point: it simulates the corruption the pop check exists
    /// for.
    #[cfg(test)]
    pub(crate) fn inject_free_raw(&self, raw: u16) {
        if let Ok(mut pool) = self.standard.lock() {
            pool.t1.free.push(raw);
        }
    }

    /// Spends every standard slot (both halves, free lists and heads) — the
    /// T7/A8 refusal test.
    #[cfg(test)]
    pub(crate) fn exhaust_standard(&self) {
        if let Ok(mut pool) = self.standard.lock() {
            pool.t1.free.clear();
            pool.t1.head = pool.t1.top + 1;
            pool.t2.free.clear();
            pool.t2.head = pool.t2.top + 1;
        }
    }
}

/// Where a spawn contextually prefers to put a monster, before any pool is
/// touched (design doc §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpawnContext {
    /// Map 0's field groups — today's only context, Tier 1.
    WorldMap,
    /// A dungeon map's groups (none loaded yet): they prefer Tier 2 so dungeon
    /// density does not eat the field's band head (§3.3).
    Dungeon,
}

/// The pool a spawn asks for (design doc §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpawnTier {
    Tier1,
    Tier2,
    Tier3,
}

/// Tier assignment, the pure function T12 pins: a level at or above
/// `tier3_level_floor` goes Tier 3 when the floor is set, otherwise the
/// context decides — field spawns Tier 1, dungeon spawns Tier 2 (§3.3). The
/// exhaustion fallback (Tier 3 spent → the context's standard tier) lives at
/// the call site in `MonsterWorld::spawn_one`, where the pools are.
pub(crate) fn spawn_tier(
    level: i32,
    context: SpawnContext,
    tier3_level_floor: Option<u16>,
) -> SpawnTier {
    if tier3_level_floor.is_some_and(|floor| level >= i32::from(floor)) {
        SpawnTier::Tier3
    } else {
        match context {
            SpawnContext::WorldMap => SpawnTier::Tier1,
            SpawnContext::Dungeon => SpawnTier::Tier2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// T9 — pool exclusivity and the band matrix (design doc §7.2): interleave
    /// the allocators to exhaustion and assert no id is ever issued twice,
    /// every id lands in its own band, the player band is never touched, and
    /// exhaustion refuses instead of panicking (A8).
    #[test]
    fn t9_pools_are_exclusive_to_their_bands_and_refuse_on_exhaustion() {
        let ids = IdAllocator::new();
        let mut seen: HashSet<u16> = HashSet::new();

        // Tier 3 first, while the standard pools are untouched: every id must
        // be inside 30_000..=59_999, and above the player band by a mile.
        let mut tier3_count = 0;
        while let Some(id) = ids.alloc_tier3() {
            assert!(
                (T3_FLOOR..=T3_TOP).contains(&id.u16()),
                "tier-3 id {} outside its band",
                id
            );
            assert!(seen.insert(id.u16()), "duplicate id {id}");
            tier3_count += 1;
        }
        assert_eq!(tier3_count, usize::from(T3_TOP - T3_FLOOR) + 1);
        assert!(ids.alloc_tier3().is_none(), "spent tier-3 refuses (A8)");

        // Standard: alloc_tier1 borrows T2 when T1 is spent, so one drain
        // empties both halves — no id in the player band, none in Tier 3,
        // none above the live wall.
        let mut standard_count = 0;
        while let Some(id) = ids.alloc_tier1() {
            assert!(
                (NPC_CHAR_UNIQUE_BASE..=T2_TOP).contains(&id.u16()),
                "standard id {} outside the standard band",
                id
            );
            assert!(seen.insert(id.u16()), "duplicate id {id}");
            standard_count += 1;
        }
        let t1_span =
            usize::from(T1_TOP - (NPC_CHAR_UNIQUE_BASE + NPC_PLACEMENTS.len() as u16)) + 1;
        let t2_span = usize::from(T2_TOP - T2_FLOOR) + 1;
        assert_eq!(standard_count, t1_span + t2_span);
        assert!(ids.alloc_tier1().is_none(), "spent standard refuses (A8)");
        assert!(ids.alloc_tier2().is_none(), "spent standard refuses (A8)");

        // Nothing was ever minted at or below the player band's top, nor from
        // the other domain's window.
        assert!(seen.iter().all(|raw| *raw > PLAYER_BAND_TOP as u16));
    }

    /// T10 — the §3.5 mutual buffering: a drained tier borrows its sibling's
    /// head, the borrowed id is a `StandardMonsterId` in the sibling's range,
    /// and freeing it returns it to the *origin* pool by range.
    #[test]
    fn t10_tiers_borrow_both_ways_and_free_by_origin() {
        // T1 borrows T2: drain T1 only (stop at the first T2-range id), then
        // the next request must come out of T2's head.
        let ids = IdAllocator::new();
        let drained = std::iter::from_fn(|| ids.alloc_tier1())
            .take_while(|id| id.u16() < T2_FLOOR)
            .count();
        let t1_span =
            usize::from(T1_TOP - (NPC_CHAR_UNIQUE_BASE + NPC_PLACEMENTS.len() as u16)) + 1;
        assert_eq!(drained, t1_span, "T1's own head is spent first");
        let borrowed = ids.alloc_tier1().expect("T1 borrows T2");
        assert!(
            (T2_FLOOR..=T2_TOP).contains(&borrowed.u16()),
            "the borrowed id is T2's, got {borrowed}"
        );
        ids.free_monster(MonsterEntityId::Standard(borrowed));
        let (t1_free, t2_free, _) = ids.free_snapshot();
        assert!(
            !t1_free.contains(&borrowed.u16()) && t2_free.contains(&borrowed.u16()),
            "a borrowed T2 id returns to T2, not T1"
        );

        // And the mirror: T2 borrows T1, the borrowed id returns to T1.
        let ids = IdAllocator::new();
        let drained = std::iter::from_fn(|| ids.alloc_tier2())
            .take_while(|id| id.u16() >= T2_FLOOR)
            .count();
        let t2_span = usize::from(T2_TOP - T2_FLOOR) + 1;
        assert_eq!(drained, t2_span, "T2's own head is spent first");
        let borrowed = ids.alloc_tier2().expect("T2 borrows T1");
        assert!(
            (NPC_CHAR_UNIQUE_BASE..=T1_TOP).contains(&borrowed.u16()),
            "the borrowed id is T1's, got {borrowed}"
        );
        ids.free_monster(MonsterEntityId::Standard(borrowed));
        let (t1_free, t2_free, _) = ids.free_snapshot();
        assert!(
            t1_free.contains(&borrowed.u16()) && !t2_free.contains(&borrowed.u16()),
            "a borrowed T1 id returns to T1, not T2"
        );

        // A recycled id is handed straight back out (free list before head).
        assert_eq!(
            ids.alloc_tier1().map(|id| id.u16()),
            Some(borrowed.u16()),
            "the free list feeds the next T1 draw"
        );
    }

    /// T11 — the type gates (design doc §5.0, spec 3): every constructor
    /// refuses `0`, negative, and foreign-band values, the two monster domains
    /// are disjoint, and the SI16 conversion refuses Tier 3 above the wire cap.
    #[test]
    fn t11_type_constructors_refuse_foreign_and_out_of_band_values() {
        // Player band, both walls, both sides.
        assert!(PlayerCharUnique::try_from_si16(0).is_err());
        assert!(PlayerCharUnique::try_from_si16(-1).is_err());
        assert_eq!(PlayerCharUnique::try_from_si16(1).map(|p| p.si16()), Ok(1));
        assert_eq!(
            PlayerCharUnique::try_from_si16(PLAYER_BAND_TOP).map(|p| p.si16()),
            Ok(PLAYER_BAND_TOP)
        );
        assert!(PlayerCharUnique::try_from_si16(PLAYER_BAND_TOP + 1).is_err());
        assert!(PlayerCharUnique::try_from_si16(i16::MAX).is_err());

        // Standard band: refuses the player band below and the wall above.
        assert!(StandardMonsterId::try_from_u16(PLAYER_BAND_TOP as u16).is_err());
        assert!(StandardMonsterId::try_from_u16(NPC_CHAR_UNIQUE_BASE - 1).is_err());
        assert!(StandardMonsterId::try_from_u16(NPC_CHAR_UNIQUE_BASE).is_ok());
        assert!(StandardMonsterId::try_from_u16(T2_TOP).is_ok());
        assert!(StandardMonsterId::try_from_u16(T2_TOP + 1).is_err());

        // Tier 3 band: refuses the standard band below and T3's own wall above.
        assert!(Tier3MonsterId::try_from_u16(T2_TOP).is_err());
        assert!(Tier3MonsterId::try_from_u16(T3_FLOOR).is_ok());
        assert!(Tier3MonsterId::try_from_u16(T3_TOP).is_ok());
        assert!(Tier3MonsterId::try_from_u16(T3_TOP + 1).is_err());

        // The SI16 gate (invariant 7): at or below the wire cap is fine, one
        // above it refuses — D9 replaces the planned "P4 stage" with a single
        // one-shot `u32` widening, so this refusal is lifted in one pass.
        let at_cap = Tier3MonsterId::try_from_u16(WIRE_SI16_TOP).expect("in band");
        assert_eq!(at_cap.try_si16(), Some(WIRE_SI16_TOP as i16));
        let over_cap = Tier3MonsterId::try_from_u16(WIRE_SI16_TOP + 1).expect("in band");
        assert_eq!(over_cap.try_si16(), None);
        // The enum forwards both cases.
        assert_eq!(
            MonsterEntityId::Tier3(over_cap).si16(),
            None,
            "the storage enum carries the wire-cap refusal"
        );
        assert_eq!(
            MonsterEntityId::Tier3(at_cap).si16(),
            Some(WIRE_SI16_TOP as i16)
        );
    }

    /// T12 — tier assignment (design doc §3.3): the floor wins when it is
    /// set, the context decides while it is off, and the shipped default
    /// keeps the floor off.
    #[test]
    fn t12_spawn_tier_follows_the_floor_then_the_context() {
        // Shipped default: the floor is off, so every context stays standard.
        assert_eq!(
            spawn_tier(9_999, SpawnContext::WorldMap, None),
            SpawnTier::Tier1
        );
        assert_eq!(
            spawn_tier(9_999, SpawnContext::Dungeon, None),
            SpawnTier::Tier2
        );
        // With a floor set: level >= floor goes Tier 3, from either context.
        assert_eq!(
            spawn_tier(100, SpawnContext::WorldMap, Some(100)),
            SpawnTier::Tier3
        );
        assert_eq!(
            spawn_tier(101, SpawnContext::Dungeon, Some(100)),
            SpawnTier::Tier3
        );
        // Below the floor falls back to the context's tier.
        assert_eq!(
            spawn_tier(99, SpawnContext::WorldMap, Some(100)),
            SpawnTier::Tier1
        );
        assert_eq!(
            spawn_tier(99, SpawnContext::Dungeon, Some(100)),
            SpawnTier::Tier2
        );
        // The allocator's own default matches the shipped switch position.
        assert_eq!(IdAllocator::new().tier3_level_floor(), None);
    }
}
