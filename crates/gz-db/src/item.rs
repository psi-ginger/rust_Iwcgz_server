//! Item storage (legacy `tbl_Person`'s item columns), scoped to the array the
//! client sees as one character's inventory.
//!
//! Two facts drive the shape of this table:
//!
//! * A slot is addressed by its **absolute** index into `cltPItemInfo::clItem`
//!   (80 entries), because that is what `GAMEMSG_REQUEST_ITEMMOVE` puts on the
//!   wire. Equipment, bag, horse and sub-bags all live in the same flat array.
//! * Only `siUnique` and `siItemNum` have confirmed offsets in the shipped
//!   84-byte `cltItem`, so the complete record is kept in `raw` next to the two
//!   promoted columns. Nothing is lost, and the promoted columns make the table
//!   queryable.

use async_trait::async_trait;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use gz_proto::item::{CLT_ITEM_SIZE, Item, MAX_ITEM_PER_PERSON};

use crate::repository::RepositoryError;

use gz_proto::game::client_map_index;
use gz_proto::item::GroundItem;

/// One occupied slot of a character's item array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterItem {
    /// Absolute index into `cltPItemInfo::clItem` (0..80).
    pub slot: u16,
    /// Range base the slot falls in — see [`container_of`].
    pub container: u16,
    /// `cltItem.siUnique`; `0` means the slot is empty.
    pub item_unique: i32,
    /// `cltItem.siItemNum`.
    pub count: i32,
    /// The complete 84-byte record.
    pub raw: Vec<u8>,
}

/// Re-exported so callers get [`container_of`] from the same place as the rest of
/// the item API. The classification is a wire fact, which is why it lives in
/// `gz_proto::item`.
pub use gz_proto::item::container_of;

/// Where a dropped item lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DropLocation {
    /// `characters.map_id` of the dropper.
    pub map_id: i32,
    /// Map cell `x`.
    pub x: i32,
    /// Map cell `y`.
    pub y: i32,
}

/// Why a [`ItemRepository::split_slot`] call was or was not applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitStatus {
    /// The count moved into the destination (fresh stack, or a merge).
    Split,
    /// The source slot held nothing.
    SourceEmpty,
    /// The client sent a non-positive count, or more than the source holds.
    BadCount {
        /// Count currently in the source slot.
        stored: u16,
        /// Count the client asked to split off.
        requested: i32,
    },
    /// The destination holds a different item; a split cannot swap.
    DestinationMismatch {
        /// `siUnique` the destination actually holds.
        stored: u16,
        /// `siUnique` the source holds.
        claimed: u16,
    },
    /// `from == to`; there is nothing to split into.
    SelfSplit,
}

/// What a [`ItemRepository::split_slot`] call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitOutcome {
    /// Why the split was or was not applied.
    pub status: SplitStatus,
    /// Source slot contents after the call (unchanged on refusal).
    pub from: Item,
    /// Destination slot contents after the call (unchanged on refusal).
    pub to: Item,
}

/// Why a `GAMEMSG_REQUEST_INVDROP` (307) was or was not applied.
///
/// The legacy names these cases by what it sends rather than by a reason code:
/// a rejected drop is answered with 7210 carrying the server's own record, so
/// the client simply overwrites its optimistic view. Same shape here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropStatus {
    /// The stack was reduced, or removed entirely.
    Dropped,
    /// The slot held nothing, so there was nothing to drop.
    SlotEmpty,
    /// The client's record names a different item than the slot does.
    ItemMismatch {
        /// `siUnique` the slot actually holds.
        stored: u16,
        /// `siUnique` the client claimed to be dropping.
        claimed: u16,
    },
    /// The client asked to drop more than the slot holds.
    NotEnough {
        /// Count currently in the slot.
        stored: u16,
        /// Count the client asked to remove.
        requested: u16,
    },
    /// Every cell of the 5x5 box around the requested cell already holds a
    /// dropped item, so the drop was refused and the slot left alone.
    GroundBlocked {
        /// Cell the client asked to drop onto.
        x: i32,
        /// Cell the client asked to drop onto.
        y: i32,
    },
}

/// What a [`ItemRepository::drop_slot`] call did.
///
/// `current` is always the record the slot holds *after* the call, which is what
/// the legacy puts in `GAMEMSG_RESPONSE_PERSONITEMINFO` — including on the
/// rejection paths, where it is the unchanged stored record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropOutcome {
    /// Why the drop was or was not applied.
    pub status: DropStatus,
    /// The slot's contents after the call.
    pub current: Item,
    /// The removed portion, when `status` is [`DropStatus::Dropped`].
    pub dropped: Option<Item>,
    /// The ground object the drop created, when `status` is
    /// [`DropStatus::Dropped`] and the count removed was non-zero.
    ///
    /// This is what `GAMEMSG_RESPONSE_ITEMDROP` (309) has to carry to make the
    /// item appear on the map, so it is built here rather than in the service:
    /// the handle is allocated by the insert, and rebuilding the frame from
    /// `dropped` plus `DropLocation` would mean trusting the two to still agree.
    pub grounded: Option<GroundItem>,
}

/// What a [`ItemRepository::move_slot`] call actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveOutcome {
    /// `from` was empty; nothing changed.
    SourceEmpty,
    /// The records were exchanged.
    Swapped {
        /// Record now held by the source slot (what the destination held before).
        from: Item,
        /// Record now held by the destination slot (what the source held before).
        to: Item,
    },
    /// Both slots held the same item, so their counts were added into `to`.
    ///
    /// The sum is capped at [`MAX_PILE`]; any remainder stays in the source
    /// slot rather than being destroyed.
    Stacked {
        /// The resulting stack size in the destination slot.
        total: i32,
        /// The destination slot's record after the merge.
        to: Item,
        /// The source slot's record after the merge: `Item::empty()` when the
        /// merge was exact, the remainder otherwise.
        from: Item,
    },
}

/// Item storage interface.
#[async_trait]
pub trait ItemRepository: Send + Sync {
    /// Every occupied slot of `character_id`, ordered by slot.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the query fails.
    async fn list(&self, character_id: i32) -> Result<Vec<CharacterItem>, RepositoryError>;

    /// Replaces `character_id`'s whole array with `items`, in one transaction.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the write fails, or
    /// [`RepositoryError::Invalid`] if an item's slot or record length is out of
    /// range.
    async fn replace_all(
        &self,
        character_id: i32,
        items: &[CharacterItem],
    ) -> Result<(), RepositoryError>;

    /// Applies a `GAMEMSG_REQUEST_ITEMMOVE` (303) to the stored array.
    ///
    /// The client applies the move locally and never waits for a reply — the
    /// legacy opcode table has no `RESPONSE_ITEMMOVE` — so this only has to make
    /// the server's copy agree.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the transaction fails.
    async fn move_slot(
        &self,
        character_id: i32,
        from: u16,
        to: u16,
    ) -> Result<MoveOutcome, RepositoryError>;

    /// Applies a `GAMEMSG_REQUEST_ITEMSPLIT` (341): moves `requested` items out
    /// of the `from` stack into `to`.
    ///
    /// The destination picks up one of the same two cases as an itemmove
    /// destination: empty, in which case the count goes there wholesale, or
    /// holding the same item, in which case the two stacks merge with the
    /// same [`MAX_PILE`] cap and remainder semantics. A destination holding a
    /// *different* item refuses the split — the legacy reaches the same answer
    /// through `sDBRequest_MoveItem` with `Canpile = true` (both items
    /// pileable), and the DB path rejects incompatible pairs instead of
    /// swapping them.
    ///
    /// The client only sends 341 after finding a free destination slot, so the
    /// merge-destination case is for robustness, not the common path.
    ///
    /// Every outcome carries the resulting contents of *both* slots (unchanged
    /// on refusal), as [`DropOutcome::current`] does for a drop: the caller
    /// needs the authoritative pair to rewrite the client's bag with the same
    /// two `GAMEMSG_RESPONSE_PERSONITEMINFO` frames the move path sends.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the transaction fails, or
    /// [`RepositoryError::Invalid`] if a slot is out of range.
    async fn split_slot(
        &self,
        character_id: i32,
        from: u16,
        to: u16,
        requested: i32,
    ) -> Result<SplitOutcome, RepositoryError>;

    /// Applies a `GAMEMSG_REQUEST_INVDROP` (307) to the stored array.
    ///
    /// `claimed` is the client's own copy of the record it wants removed. It is
    /// validated against the stored record the way the legacy does
    /// (`Server-Item.cpp:2462-2500`) and then the requested count is subtracted,
    /// clearing the slot if the stack empties.
    ///
    /// The removed portion is written to `world_items` at `at`. Dropping it on the
    /// floor without storing it would destroy the item, because there is no
    /// world-object model to spawn it into yet.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the transaction fails, or
    /// [`RepositoryError::Invalid`] if `slot` is outside `0..MAX_ITEM_PER_PERSON`.
    async fn drop_slot(
        &self,
        character_id: i32,
        slot: u16,
        claimed: &Item,
        at: DropLocation,
    ) -> Result<DropOutcome, RepositoryError>;

    /// Applies a `GAMEMSG_REQUEST_INVPICKUP` (308): moves one ground item back
    /// into a character's inventory.
    ///
    /// `claim` is the client's description of what it clicked. The legacy checks
    /// the whole five-field tuple and not merely the handle
    /// (`Server-Item.cpp:2358-2370`): the handle must resolve to a live row, and
    /// then that row's unique, map and both cell coordinates must equal what the
    /// client sent. Those checks happen here so a stale or forged handle cannot
    /// move an item, and so the failure is attributable in the log.
    ///
    /// On success the row leaves `world_items` and the item occupies a free
    /// carry slot, both in one transaction — a crash between the two would
    /// otherwise either duplicate or destroy the item.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the transaction fails.
    async fn pickup_ground(
        &self,
        character_id: i32,
        claim: GroundClaim,
    ) -> Result<PickupOutcome, RepositoryError>;
}

/// The client's claim about which ground object was clicked — the five fields of
/// `cltGameMsgRequest_InvPickUp` (`MsgType-Item.h:389`), named for what they
/// identify rather than for their wire names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroundClaim {
    /// `siItemIndex` — the ground handle, allocated in
    /// `0005_world_item_handle.sql` and handed to the client as 309's
    /// `uiItemListIndex`.
    pub handle: i16,
    /// `siItemUnique`.
    pub item_unique: i16,
    /// `siMapIndex` — compared against [`client_map_index`] of the stored
    /// `map_id`, not against `map_id` itself, because that is the number the
    /// client is holding.
    pub map_index: i16,
    /// `siX`.
    pub x: i16,
    /// `siY`.
    pub y: i16,
}

/// Why a `GAMEMSG_REQUEST_INVPICKUP` (308) was or was not applied.
///
/// Every variant here is a refusal, and the legacy answers all of them the same
/// way: with nothing. `DoMsg_GAMEMSG_REQUEST_INVPICKUP` returns before reaching
/// any send, so a stale click costs the client silence rather than a correction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickupStatus {
    /// The item moved into the character's inventory.
    PickedUp,
    /// No live ground item carries that handle.
    NoSuchItem {
        /// The handle the client claimed.
        handle: i16,
    },
    /// The handle resolved, but the row's `siUnique` is not the one claimed. The
    /// two come from separate per-cell fields, so this catches a client that has
    /// a stale unique for a live cell.
    ItemMismatch {
        /// `siUnique` the stored row holds.
        stored: i32,
        /// `siUnique` the client claimed.
        claimed: i16,
    },
    /// The handle resolved and the unique matched, but the row is not where the
    /// client says it is. Together with the unique this pins all five fields.
    CellMismatch {
        /// Where the stored row actually is, as `(map index, x, y)`.
        stored: (i16, i32, i32),
        /// What the client claimed, as `(map index, x, y)`.
        claimed: (i16, i16, i16),
    },
    /// Every carry slot is occupied.
    NoFreeSlot,
}

/// What a [`ItemRepository::pickup_ground`] call did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickupOutcome {
    /// Whether the item moved.
    pub status: PickupStatus,
    /// The slot the item landed in, when `status` is [`PickupStatus::PickedUp`].
    pub slot: Option<u16>,
    /// The item as it now sits in that slot, when `status` is
    /// [`PickupStatus::PickedUp`].
    pub item: Option<Item>,
    /// The frame that clears the cell's ground icon, when `status` is
    /// [`PickupStatus::PickedUp`].
    ///
    /// `GAMEMSG_RESPONSE_ITEMDROP` (309) doubles as the removal: the legacy sends
    /// the item's own coordinates with both handles zeroed
    /// (`ServerMap.cpp:288`, `SendDelItemMsgToOthers`). It is built here because
    /// it needs the stored coordinates rather than the client's claim.
    pub cleared: Option<GroundItem>,
}

/// PostgreSQL implementation. Cheap to clone (shares the pool).
#[derive(Clone)]
pub struct PgItemRepository {
    pool: PgPool,
}

impl PgItemRepository {
    /// Wraps an existing pool (e.g. one shared with the character repository).
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Opens a fresh pool for `database_url`.
    ///
    /// # Errors
    ///
    /// [`RepositoryError::Database`] if the pool cannot connect.
    pub async fn connect(database_url: &str) -> Result<Self, RepositoryError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }
}

const SELECT_COLUMNS: &str =
    "SELECT slot, container, item_unique, count, raw FROM character_items WHERE character_id = $1";

/// `clPos.siMapIndex` for a stored `map_id`, as the client will be holding it.
///
/// The 309 handler discards the frame unless this equals the client's
/// `CurrentMapIndex`, which it took from `RESPONSE_LOGIN` — so the two have to be
/// the same number, and [`client_map_index`] is what both go through.
fn map_index_for(map_id: i32) -> Result<i16, RepositoryError> {
    i16::try_from(client_map_index(map_id)).map_err(|_| {
        RepositoryError::Invalid(format!(
            "map {map_id} does not fit the SI16 clPos.siMapIndex"
        ))
    })
}

/// One map-cell axis narrowed to the `SI16` that 309 carries.
///
/// `field` names the `clPos` member for the error message.
///
/// # Errors
///
/// [`RepositoryError::Invalid`] if `value` is outside `SI16`. A cell outside the
/// client's `MapXsize`/`MapYsize` is dropped without a trace there, so this fails
/// loudly here instead.
fn cell_axis(field: &str, value: i32) -> Result<i16, RepositoryError> {
    i16::try_from(value).map_err(|_| {
        RepositoryError::Invalid(format!(
            "ground item {field}={value} does not fit the SI16 it travels as"
        ))
    })
}

/// Upserts one slot, deriving `container` from the slot index so the column can
/// never drift from `gz_proto::item::container_of`.
const UPSERT_SLOT: &str = "INSERT INTO character_items \
     (character_id, slot, container, item_unique, count, raw) \
     VALUES ($1, $2, $3, $4, $5, $6) \
     ON CONFLICT (character_id, slot) DO UPDATE \
     SET container = EXCLUDED.container, \
         item_unique = EXCLUDED.item_unique, \
         count = EXCLUDED.count, \
         raw = EXCLUDED.raw";

const DELETE_SLOT: &str = "DELETE FROM character_items WHERE character_id = $1 AND slot = $2";

/// One stored slot as `move_slot` needs it.
type SlotRow = (i32, i32, Vec<u8>);

/// Reads one slot inside an open transaction.
async fn read_slot(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    character_id: i32,
    slot: u16,
) -> Result<Option<SlotRow>, sqlx::Error> {
    sqlx::query_as::<_, SlotRow>(
        "SELECT item_unique, count, raw FROM character_items \
         WHERE character_id = $1 AND slot = $2",
    )
    .bind(character_id)
    .bind(i16::try_from(slot).unwrap_or(0))
    .fetch_optional(&mut **tx)
    .await
}

/// Writes one slot inside an open transaction.
async fn write_slot(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    character_id: i32,
    slot: u16,
    unique: i32,
    count: i32,
    raw: &[u8],
) -> Result<(), sqlx::Error> {
    sqlx::query(UPSERT_SLOT)
        .bind(character_id)
        .bind(i16::try_from(slot).unwrap_or(0))
        .bind(i16::try_from(container_of(slot)).unwrap_or(0))
        .bind(unique)
        .bind(count)
        .bind(raw.to_vec())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// The maximum stack size a pileable item can hold. Pickup stacking merges only
/// when the whole incoming stack fits, and move-merge caps the destination at
/// this and leaves the overflow in the source slot.
const MAX_PILE: i32 = 30000;

/// Whether two slots holding the same `siUnique` merge into one stack.
///
/// The legacy merges only when the item is pileable: `cltItem::CanPile`
/// (`ItemUnit.cpp:1458`) is false when the item's type carries `ITEMATB_OBJECT`,
/// which is every weapon, armour piece and accessory (the `ITEMATB_OBJECT` types
/// in `cltItemManagerCommon::SetItemType`, `cltItem-SetItemData.Cpp`).
/// `gz_data::is_pileable` carries the resulting per-unique answer, so equipment
/// in any slot -- equipped or in a bag -- does not stack.
fn stackable(item_unique: i32) -> bool {
    u16::try_from(item_unique).is_ok_and(gz_data::is_pileable)
}

/// Clears one slot inside an open transaction.
async fn clear_slot(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    character_id: i32,
    slot: u16,
) -> Result<(), sqlx::Error> {
    sqlx::query(DELETE_SLOT)
        .bind(character_id)
        .bind(i16::try_from(slot).unwrap_or(0))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// The slots a picked-up item may land in, in the order the legacy would try
/// them.
///
/// The legacy asks `CanAddInv(INVMODE_ALL, ...)`, which considers every container
/// a character can carry in. Equipment is excluded because the request is to
/// *carry* the item, not to equip it — the client has not said which equipment
/// slot it wants, so there is nothing to write one into. That leaves the main bag,
/// the horse and both extra bags, in ascending slot order.
fn carry_slots() -> impl Iterator<Item = u16> {
    use gz_proto::item::personitem::*;
    (INV0..=INV_END)
        .chain(HORSE0..=HORSE_END)
        .chain(BAG0..=BAG0_END)
        .chain(BAG1..=BAG1_END)
}

/// The 24 cells of the 5x5 box around a requested drop cell, nearest ring
/// first and within each ring in the legacy's scan order (dy then dx,
/// ascending) — the exact preference of `FindProperAreaForDrop`
/// (`MapManager.cpp:999`), minus its map-bounds and collision checks, which
/// this server has no map model for.
fn neighbor_cells(x: i32, y: i32) -> impl Iterator<Item = (i32, i32)> {
    let ring = |d: i32| {
        (-d..=d).flat_map(move |dy| {
            (-d..=d)
                .filter(move |dx| dx.abs() == d || dy.abs() == d)
                .map(move |dx| (x + dx, y + dy))
        })
    };
    ring(1)
        .chain(ring(2))
        .filter(|(nx, ny)| *nx >= 0 && *ny >= 0)
}

/// The cells a drop tries, requested cell first then the other 24 cells of the
/// 5x5 box around it, so an empty target cell always wins and a crowded box
/// resolves in the legacy's nearest-first order (see [`neighbor_cells`]).
fn drop_candidates(at: DropLocation) -> impl Iterator<Item = DropLocation> {
    std::iter::once(at).chain(neighbor_cells(at.x, at.y).map(move |(x, y)| DropLocation {
        map_id: at.map_id,
        x,
        y,
    }))
}

/// Finds the lowest free carry slot inside an open transaction.
///
/// "Free" means no row, or a row whose unique is zero. The table only ever holds
/// occupied slots — [`write_slot`] clears by deleting — so in practice this tests
/// for absence, but a zero-unique row would render as an empty cell to the client
/// and must not be treated as occupied.
///
/// # Errors
///
/// [`sqlx::Error`] if the read fails.
async fn find_free_carry_slot(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    character_id: i32,
) -> Result<Option<u16>, sqlx::Error> {
    // One row per occupied slot, so the complement of this set is what is free.
    let occupied: Vec<i16> = sqlx::query_scalar(
        "SELECT slot FROM character_items WHERE character_id = $1 AND item_unique <> 0",
    )
    .bind(character_id)
    .fetch_all(&mut **tx)
    .await?;

    Ok(carry_slots().find(|slot| !occupied.contains(&(*slot as i16))))
}

#[async_trait]
impl ItemRepository for PgItemRepository {
    async fn list(&self, character_id: i32) -> Result<Vec<CharacterItem>, RepositoryError> {
        type Row = (i16, i16, i32, i32, Vec<u8>);
        let rows = sqlx::query_as::<_, Row>(&format!("{SELECT_COLUMNS} ORDER BY slot"))
            .bind(character_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|(slot, container, item_unique, count, raw)| CharacterItem {
                slot: u16::try_from(slot).unwrap_or(0),
                container: u16::try_from(container).unwrap_or(0),
                item_unique,
                count,
                raw,
            })
            .collect())
    }

    async fn replace_all(
        &self,
        character_id: i32,
        items: &[CharacterItem],
    ) -> Result<(), RepositoryError> {
        for item in items {
            if usize::from(item.slot) >= MAX_ITEM_PER_PERSON {
                return Err(RepositoryError::Invalid(format!(
                    "item slot {} out of range 0..{MAX_ITEM_PER_PERSON}",
                    item.slot
                )));
            }
            if item.raw.len() != CLT_ITEM_SIZE {
                return Err(RepositoryError::Invalid(format!(
                    "item record for slot {} is {} bytes, expected {CLT_ITEM_SIZE}",
                    item.slot,
                    item.raw.len()
                )));
            }
        }

        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM character_items WHERE character_id = $1")
            .bind(character_id)
            .execute(&mut *tx)
            .await?;
        for item in items {
            sqlx::query(
                "INSERT INTO character_items \
                 (character_id, slot, container, item_unique, count, raw) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(character_id)
            .bind(i16::try_from(item.slot).unwrap_or(0))
            .bind(i16::try_from(item.container).unwrap_or(0))
            .bind(item.item_unique)
            .bind(item.count)
            .bind(&item.raw)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn move_slot(
        &self,
        character_id: i32,
        from: u16,
        to: u16,
    ) -> Result<MoveOutcome, RepositoryError> {
        let bound = MAX_ITEM_PER_PERSON as u16;
        if u32::from(from) >= u32::from(bound) || u32::from(to) >= u32::from(bound) {
            return Err(RepositoryError::Invalid(format!(
                "item move {from}->{to} is outside 0..{MAX_ITEM_PER_PERSON}"
            )));
        }
        let mut tx = self.pool.begin().await?;

        let Some((src_unique, src_count, src_raw)) = read_slot(&mut tx, character_id, from).await?
        else {
            tx.rollback().await?;
            return Ok(MoveOutcome::SourceEmpty);
        };

        // A self-move is a no-op; report the stored record back so the service
        // can re-assert the slot to the client.
        if from == to {
            tx.rollback().await?;
            let item = Item::decode(&src_raw).map_err(|e| {
                RepositoryError::Invalid(format!("stored record for slot {from}: {e}"))
            })?;
            return Ok(MoveOutcome::Swapped {
                from: item,
                to: item,
            });
        }

        let outcome = match read_slot(&mut tx, character_id, to).await? {
            // Two stacks of the same pileable item merge into the destination,
            // which is what the client's own drag handler does locally. The sum
            // is capped at `MAX_PILE`, like the pickup stacking path; the
            // overflow stays in the source slot instead of being dropped.
            // Equipment does not stack, so a same-unique move of it falls
            // through to the swap arm below.
            Some((dst_unique, dst_count, dst_raw))
                if dst_unique == src_unique && stackable(dst_unique) =>
            {
                let src_item = Item::decode(&src_raw).map_err(|e| {
                    RepositoryError::Invalid(format!("stored record for slot {from}: {e}"))
                })?;
                let dst_item = Item::decode(&dst_raw).map_err(|e| {
                    RepositoryError::Invalid(format!("stored record for slot {to}: {e}"))
                })?;
                let total = dst_count.saturating_add(src_count);
                let (dst_new, src_rest) = if total > MAX_PILE {
                    (MAX_PILE, total - MAX_PILE)
                } else {
                    (total, 0)
                };

                let mut merged = dst_item;
                merged.set_count(dst_new as u16);
                write_slot(
                    &mut tx,
                    character_id,
                    to,
                    dst_unique,
                    dst_new,
                    &merged.encode(),
                )
                .await?;

                let from_item = if src_rest == 0 {
                    clear_slot(&mut tx, character_id, from).await?;
                    Item::empty()
                } else {
                    let mut left = src_item;
                    left.set_count(src_rest as u16);
                    write_slot(
                        &mut tx,
                        character_id,
                        from,
                        src_unique,
                        src_rest,
                        &left.encode(),
                    )
                    .await?;
                    left
                };
                MoveOutcome::Stacked {
                    total: dst_new,
                    to: merged,
                    from: from_item,
                }
            }
            // Otherwise the two records simply exchange places, so an occupied
            // destination needs no special case.
            Some((dst_unique, dst_count, dst_raw)) => {
                let dst_item = Item::decode(&dst_raw).map_err(|e| {
                    RepositoryError::Invalid(format!("stored record for slot {to}: {e}"))
                })?;
                let src_item = Item::decode(&src_raw).map_err(|e| {
                    RepositoryError::Invalid(format!("stored record for slot {from}: {e}"))
                })?;
                write_slot(&mut tx, character_id, from, dst_unique, dst_count, &dst_raw).await?;
                write_slot(&mut tx, character_id, to, src_unique, src_count, &src_raw).await?;
                MoveOutcome::Swapped {
                    from: dst_item,
                    to: src_item,
                }
            }
            None => {
                let src_item = Item::decode(&src_raw).map_err(|e| {
                    RepositoryError::Invalid(format!("stored record for slot {from}: {e}"))
                })?;
                write_slot(&mut tx, character_id, to, src_unique, src_count, &src_raw).await?;
                clear_slot(&mut tx, character_id, from).await?;
                MoveOutcome::Swapped {
                    from: Item::empty(),
                    to: src_item,
                }
            }
        };

        tx.commit().await?;
        Ok(outcome)
    }

    async fn split_slot(
        &self,
        character_id: i32,
        from: u16,
        to: u16,
        requested: i32,
    ) -> Result<SplitOutcome, RepositoryError> {
        let bound = MAX_ITEM_PER_PERSON as u16;
        if u32::from(from) >= u32::from(bound) || u32::from(to) >= u32::from(bound) {
            return Err(RepositoryError::Invalid(format!(
                "item split {from}->{to} is outside 0..{MAX_ITEM_PER_PERSON}"
            )));
        }

        let mut tx = self.pool.begin().await?;

        let Some((src_unique, src_count, src_raw)) = read_slot(&mut tx, character_id, from).await?
        else {
            tx.rollback().await?;
            return Ok(SplitOutcome {
                status: SplitStatus::SourceEmpty,
                from: Item::empty(),
                to: Item::empty(),
            });
        };
        let src_item = Item::decode(&src_raw)
            .map_err(|e| RepositoryError::Invalid(format!("stored record for slot {from}: {e}")))?;
        let dest_slot = read_slot(&mut tx, character_id, to).await?;

        // A self-split rearranges nothing, but answering it as success would
        // also double the apparent stack size if the client ever merged
        // the reply; refuse it cleanly instead.
        if from == to {
            tx.rollback().await?;
            return Ok(SplitOutcome {
                status: SplitStatus::SelfSplit,
                from: src_item,
                to: src_item,
            });
        }

        let stored_count = u16::try_from(src_count).unwrap_or(0);
        if requested <= 0 || requested > i32::from(stored_count) {
            let current_to = match &dest_slot {
                Some((_, _, raw)) => Item::decode(raw).map_err(|e| {
                    RepositoryError::Invalid(format!("stored record for slot {to}: {e}"))
                })?,
                None => Item::empty(),
            };
            tx.rollback().await?;
            return Ok(SplitOutcome {
                status: SplitStatus::BadCount {
                    stored: stored_count,
                    requested,
                },
                from: src_item,
                to: current_to,
            });
        }
        let outcome = match dest_slot {
            Some((dst_unique, dst_count, dst_raw)) => {
                let dst_unique = u16::try_from(dst_unique).unwrap_or(0);
                // Same item, but a non-pileable item (equipment) cannot be
                // merged either: refuse rather than pile two swords.
                if dst_unique != u16::try_from(src_unique).unwrap_or(0) || !stackable(src_unique) {
                    tx.rollback().await?;
                    return Ok(SplitOutcome {
                        status: SplitStatus::DestinationMismatch {
                            stored: dst_unique,
                            claimed: u16::try_from(src_unique).unwrap_or(0),
                        },
                        from: src_item,
                        to: Item::decode(&dst_raw).map_err(|e| {
                            RepositoryError::Invalid(format!("stored record for slot {to}: {e}"))
                        })?,
                    });
                }

                // Same-item destination: merge, capped at `MAX_PILE`, the
                // excess of the split off staying in the source.
                let dst_item = Item::decode(&dst_raw).map_err(|e| {
                    RepositoryError::Invalid(format!("stored record for slot {to}: {e}"))
                })?;
                let dst_count = u16::try_from(dst_count).unwrap_or(0);
                let total = i32::from(dst_count) + requested;
                let dst_new = if total > MAX_PILE { MAX_PILE } else { total };
                // The destination absorbed only `dst_new - dst_count` from the
                // source, so that many of the source's stored items leave it.
                // (`total - MAX_PILE` would undercount the survivor when the
                // legacy-style whole-stack exception does not apply.)
                let src_rest = i32::from(stored_count) - (dst_new - i32::from(dst_count));

                let mut merged = dst_item;
                merged.set_count(dst_new as u16);
                write_slot(
                    &mut tx,
                    character_id,
                    to,
                    src_unique,
                    dst_new,
                    &merged.encode(),
                )
                .await?;

                let from_item = if src_rest == 0 {
                    clear_slot(&mut tx, character_id, from).await?;
                    Item::empty()
                } else {
                    let mut left = src_item;
                    left.set_count(src_rest as u16);
                    write_slot(
                        &mut tx,
                        character_id,
                        from,
                        src_unique,
                        src_rest,
                        &left.encode(),
                    )
                    .await?;
                    left
                };
                SplitOutcome {
                    status: SplitStatus::Split,
                    from: from_item,
                    to: merged,
                }
            }
            None => {
                // Empty destination: the split lands whole there.
                let mut moved = src_item;
                moved.set_count(requested as u16);
                write_slot(
                    &mut tx,
                    character_id,
                    to,
                    src_unique,
                    requested,
                    &moved.encode(),
                )
                .await?;

                let remaining = i32::from(stored_count) - requested;
                let from_item = if remaining == 0 {
                    clear_slot(&mut tx, character_id, from).await?;
                    Item::empty()
                } else {
                    let mut left = src_item;
                    left.set_count(remaining as u16);
                    write_slot(
                        &mut tx,
                        character_id,
                        from,
                        src_unique,
                        remaining,
                        &left.encode(),
                    )
                    .await?;
                    left
                };
                SplitOutcome {
                    status: SplitStatus::Split,
                    from: from_item,
                    to: moved,
                }
            }
        };

        tx.commit().await?;
        Ok(outcome)
    }

    async fn drop_slot(
        &self,
        character_id: i32,
        slot: u16,
        claimed: &Item,
        at: DropLocation,
    ) -> Result<DropOutcome, RepositoryError> {
        if u32::from(slot) >= MAX_ITEM_PER_PERSON as u32 {
            return Err(RepositoryError::Invalid(format!(
                "item drop slot {slot} is outside 0..{MAX_ITEM_PER_PERSON}"
            )));
        }

        let mut tx = self.pool.begin().await?;

        let Some((stored_unique, stored_count, stored_raw)) =
            read_slot(&mut tx, character_id, slot).await?
        else {
            tx.rollback().await?;
            return Ok(DropOutcome {
                status: DropStatus::SlotEmpty,
                current: Item::empty(),
                dropped: None,
                grounded: None,
            });
        };

        // The table holds `i32` because the legacy `siUnique`/`siItemNum` are
        // signed, but the shipped record narrows both to 16 bits.
        let stored_unique = u16::try_from(stored_unique).unwrap_or(0);
        let stored_count = u16::try_from(stored_count).unwrap_or(0);
        let stored = Item::decode(&stored_raw)
            .map_err(|e| RepositoryError::Invalid(format!("stored record for slot {slot}: {e}")))?;

        // `cltItem::IsSame(claimed, false)` (`ItemUnit.cpp:3178`) compares
        // `siUnique`, the whole `siData32` union and the date/element fields, but
        // *not* the count. Only `siUnique` has a confirmed offset in the shipped
        // 84-byte record (see `gz_proto::item`), and comparing bytes we have not
        // mapped would be guessing, so the check is limited to what is known. In
        // practice the client's copy is byte-identical to ours anyway: 249 is
        // spliced from these same rows.
        if stored_unique != claimed.unique() {
            tx.rollback().await?;
            return Ok(DropOutcome {
                status: DropStatus::ItemMismatch {
                    stored: stored_unique,
                    claimed: claimed.unique(),
                },
                current: stored,
                dropped: None,
                grounded: None,
            });
        }

        let requested = claimed.count();
        if stored_count < requested {
            tx.rollback().await?;
            return Ok(DropOutcome {
                status: DropStatus::NotEnough {
                    stored: stored_count,
                    requested,
                },
                current: stored,
                dropped: None,
                grounded: None,
            });
        }

        let remaining = stored_count - requested;
        let mut dropped = stored;
        dropped.set_count(requested);

        // The client keeps a single item per ground cell
        // (`cltMapCommon::SetItemMap` overwrites), and `ITZMap::AddItem`
        // (`ITZMap.cpp:283`) returns FALSE on an occupied cell — so an item
        // dropped onto an occupied cell renders nowhere and cannot be picked
        // up: it is lost. Mirror the legacy's recovery by landing on a free
        // neighbouring cell instead; without this the item is destroyed.
        //
        // The cell is chosen by the INSERT itself, not by a prior read:
        // `world_items (map_id, x, y)` is UNIQUE (sql/0010) and every attempt is
        // `ON CONFLICT DO NOTHING`, so a cell a concurrent drop just took simply
        // yields the next candidate. There is no check-then-insert window left
        // for two drops to race into the same cell.
        let mut target = at;
        let mut obj_index = None;
        // A zero count would violate `world_items.count > 0`; the legacy guards
        // the same case by sending `clChangedItem` only when both fields are
        // non-zero (`ServerDB-Item.cpp:2810`).
        if requested > 0 {
            let mut landed: Option<(DropLocation, i32)> = None;
            for spot in drop_candidates(at) {
                let inserted: Option<i32> = sqlx::query_scalar(
                    "INSERT INTO world_items \
                     (item_unique, count, raw, map_id, x, y, dropped_by, obj_index) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, nextval('world_items_obj_index_seq')) \
                     ON CONFLICT (map_id, x, y) DO NOTHING \
                     RETURNING obj_index",
                )
                .bind(i32::from(stored_unique))
                .bind(i32::from(requested))
                .bind(dropped.encode())
                .bind(spot.map_id)
                .bind(spot.x)
                .bind(spot.y)
                .bind(character_id)
                .fetch_optional(&mut *tx)
                .await?;
                if let Some(list_index) = inserted {
                    landed = Some((spot, list_index));
                    break;
                }
            }
            match landed {
                Some((spot, list_index)) => {
                    target = spot;
                    obj_index = Some(u16::try_from(list_index).map_err(|_| {
                        RepositoryError::Invalid(format!(
                            "ground item handle {list_index} does not fit the UI16 that \
                             GAMEMSG_RESPONSE_ITEMDROP carries"
                        ))
                    })?);
                }
                None => {
                    tx.rollback().await?;
                    return Ok(DropOutcome {
                        status: DropStatus::GroundBlocked { x: at.x, y: at.y },
                        current: stored,
                        dropped: None,
                        grounded: None,
                    });
                }
            }
        }

        // `GAMEMSG_RESPONSE_ITEMDROP` names the cell in signed 16-bit words, and the
        // client bounds-checks x/y against its own map dimensions before drawing.
        // A cell we cannot represent would be dropped silently there, so refuse
        // the drop here where it is still visible.
        let grounded = match obj_index {
            None => None,
            Some(list_index) => Some(GroundItem {
                map_index: map_index_for(target.map_id)?,
                x: cell_axis("clPos.siX", target.x)?,
                y: cell_axis("clPos.siY", target.y)?,
                item_unique: stored_unique,
                list_index,
            }),
        };

        let current = if remaining == 0 {
            clear_slot(&mut tx, character_id, slot).await?;
            Item::empty()
        } else {
            let mut left = stored;
            left.set_count(remaining);
            write_slot(
                &mut tx,
                character_id,
                slot,
                stored_unique.into(),
                i32::from(remaining),
                &left.encode(),
            )
            .await?;
            left
        };

        tx.commit().await?;
        Ok(DropOutcome {
            status: DropStatus::Dropped,
            current,
            dropped: Some(dropped),
            grounded,
        })
    }

    async fn pickup_ground(
        &self,
        character_id: i32,
        claim: GroundClaim,
    ) -> Result<PickupOutcome, RepositoryError> {
        let mut tx = self.pool.begin().await?;

        // The handle is the key. Everything after this is the legacy's
        // belt-and-braces re-check of the fields the client read back out of the
        // per-cell pair that 309 wrote (`Server-Item.cpp:2358-2370`).
        let Some((world_item_id, item_unique, count, raw, map_id, x, y)) =
            sqlx::query_as::<_, (i64, i32, i32, Vec<u8>, i32, i32, i32)>(
                "SELECT world_item_id, item_unique, count, raw, map_id, x, y \
                 FROM world_items WHERE obj_index = $1",
            )
            .bind(i32::from(claim.handle))
            .fetch_optional(&mut *tx)
            .await?
        else {
            tx.rollback().await?;
            return Ok(PickupOutcome {
                status: PickupStatus::NoSuchItem {
                    handle: claim.handle,
                },
                slot: None,
                item: None,
                cleared: None,
            });
        };

        if item_unique != i32::from(claim.item_unique) {
            tx.rollback().await?;
            return Ok(PickupOutcome {
                status: PickupStatus::ItemMismatch {
                    stored: item_unique,
                    claimed: claim.item_unique,
                },
                slot: None,
                item: None,
                cleared: None,
            });
        }

        // The client holds the *masked* map index, so that is what it is compared
        // against -- see `map_index_for`.
        let stored_map = map_index_for(map_id)?;
        if stored_map != claim.map_index || x != i32::from(claim.x) || y != i32::from(claim.y) {
            tx.rollback().await?;
            return Ok(PickupOutcome {
                status: PickupStatus::CellMismatch {
                    stored: (stored_map, x, y),
                    claimed: (claim.map_index, claim.x, claim.y),
                },
                slot: None,
                item: None,
                cleared: None,
            });
        }

        // Try to stack into an existing slot that holds the same item and has room.
        let mut landed_in_existing = false;
        let slot_resolved = {
            // Check for existing stackable slot first.
            let candidates = sqlx::query_as::<_, (i16, i32, Vec<u8>)>(
        "SELECT slot, count, raw FROM character_items WHERE character_id = $1 AND item_unique = $2",
    )
    .bind(character_id)
    .bind(item_unique)
    .fetch_all(&mut *tx)
    .await?;
            let mut merge_target: Option<(u16, i32)> = None;
            // Only pileable items stack into an existing slot. Equipment -- even
            // a duplicate of an equipped item -- goes to a free carry slot,
            // exactly as the legacy `CanAddInv` (which piles only
            // `if(pileswitch)`, `ItemPerson.cpp:315`) would place it.
            if stackable(item_unique) {
                for (slot, c, _raw) in &candidates {
                    let slot = u16::try_from(*slot).unwrap_or(0);
                    let c = *c;
                    if c > 0 && c < MAX_PILE {
                        let newc = c + count;
                        if newc <= MAX_PILE {
                            merge_target = Some((slot, newc));
                            break;
                        }
                    }
                }
            }
            match merge_target {
                Some((mslot, mcount)) => {
                    // Replace the existing stack count; the raw is the item record template.
                    let itemrec = Item::decode(&raw).map_err(|e| {
                        RepositoryError::Invalid(format!(
                            "ground item {world_item_id} has an unreadable {}-byte record: {e}",
                            raw.len()
                        ))
                    })?;
                    let mut merged = itemrec;
                    merged.set_count(mcount as u16);
                    write_slot(
                        &mut tx,
                        character_id,
                        mslot,
                        item_unique,
                        mcount,
                        &merged.encode(),
                    )
                    .await?;
                    landed_in_existing = true;
                    Some(mslot)
                }
                None => find_free_carry_slot(&mut tx, character_id).await?,
            }
        };

        let Some(slot) = slot_resolved else {
            tx.rollback().await?;
            return Ok(PickupOutcome {
                status: PickupStatus::NoFreeSlot,
                slot: None,
                item: None,
                cleared: None,
            });
        };

        if !landed_in_existing {
            write_slot(&mut tx, character_id, slot, item_unique, count, &raw).await?;
        }
        // Delete the ground item either way.
        sqlx::query("DELETE FROM world_items WHERE world_item_id = $1")
            .bind(world_item_id)
            .execute(&mut *tx)
            .await?;

        let _item = Item::decode(&raw).map_err(|e| {
            RepositoryError::Invalid(format!(
                "ground item {world_item_id} has an unreadable {}-byte record: {e}",
                raw.len()
            ))
        })?;
        // We know the final count when merged; reconstruct it.
        let final_item = {
            let mut m = Item::decode(&raw).map_err(|e| {
                RepositoryError::Invalid(format!("ground item {world_item_id}: {e}"))
            })?;
            if landed_in_existing {
                let mut new_count: i32 = 0;
                if let Some((_, c)) = sqlx::query_as::<_, (i16, i32)>(
                    "SELECT slot, count FROM character_items WHERE character_id = $1 AND slot = $2",
                )
                .bind(character_id)
                .bind(i16::try_from(slot).unwrap_or(0))
                .fetch_optional(&mut *tx)
                .await?
                {
                    new_count = c;
                }
                m.set_count(u16::try_from(new_count).unwrap_or(m.count()));
            }
            m
        };
        tx.commit().await?;

        let cleared = GroundItem::removal(
            stored_map,
            cell_axis("clPos.siX", x)?,
            cell_axis("clPos.siY", y)?,
        );
        Ok(PickupOutcome {
            status: PickupStatus::PickedUp,
            slot: Some(slot),
            item: Some(final_item),
            cleared: Some(cleared),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{DropLocation, carry_slots, drop_candidates, stackable};
    use gz_proto::item::personitem::*;

    /// A pickup may land anywhere the character carries, but never in equipment:
    /// the request is to carry the item and says nothing about which equipment
    /// slot it wants.
    #[test]
    fn carry_slots_exclude_equipment() {
        let slots: Vec<u16> = carry_slots().collect();

        assert!(
            !slots.iter().any(|&s| s <= RESERVED1),
            "equipment 0..=14 must not be a pickup destination: {slots:?}"
        );
        assert!(slots.contains(&INV0) && slots.contains(&INV_END));
        assert!(slots.contains(&HORSE0) && slots.contains(&HORSE_END));
        assert!(slots.contains(&BAG0) && slots.contains(&BAG1_END));
    }

    /// The carry ranges must cover the whole array except equipment and the unused
    /// slot 79, or a pickup would be refused while a usable slot exists.
    #[test]
    fn carry_slots_cover_every_carryable_slot_once() {
        let slots: Vec<u16> = carry_slots().collect();

        let mut sorted = slots.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), slots.len(), "a slot is offered twice");

        // 0..80 minus equipment 0..=14 and the unused 79.
        assert_eq!(slots.len(), 80 - 15 - 1);
        assert!(!slots.contains(&79));
        assert!(slots.iter().all(|&s| (INV0..BAG1_END + 1).contains(&s)));
    }

    /// Lowest-slot-first, so a pickup lands in the main bag rather than the horse
    /// when both are free -- the same order `CanAddInv` searches containers in.
    #[test]
    fn carry_slots_are_offered_lowest_first() {
        let slots: Vec<u16> = carry_slots().collect();
        let mut sorted = slots.clone();
        sorted.sort_unstable();
        assert_eq!(slots, sorted);
        assert_eq!(slots[0], INV0);
    }

    /// A drop lands on the requested cell whenever that cell is free, and falls
    /// back through the rest of the 5x5 box (up to 24 further cells) otherwise —
    /// nearest ring first, in `FindProperAreaForDrop`'s scan order
    /// (`MapManager.cpp:999`). `drop_slot`'s insert loop tries exactly this
    /// sequence and reports `GroundBlocked` only when all 25 cells are taken, so
    /// pinning the stream keeps the recovery stable.
    #[test]
    fn drop_candidates_try_the_requested_cell_before_any_neighbour() {
        let at = DropLocation {
            map_id: 7,
            x: 100,
            y: 100,
        };
        let cells: Vec<DropLocation> = drop_candidates(at).collect();

        assert_eq!(cells[0], at, "the empty requested cell must win");
        assert_eq!(
            cells.len(),
            25,
            "the full 5x5 box around the requested cell"
        );

        let rest = &cells[1..];
        // Every cell of the box appears exactly once.
        let mut distinct = rest.to_vec();
        distinct.sort_unstable_by_key(|c| (c.x, c.y));
        distinct.dedup();
        assert_eq!(distinct.len(), 24);

        // Nearest ring (Chebyshev distance 1) comes before the outer ring (2).
        let chebyshev = |c: &DropLocation| (c.x - at.x).abs().max((c.y - at.y).abs());
        assert!(rest[..8].iter().all(|c| chebyshev(c) == 1));
        assert!(rest[8..].iter().all(|c| chebyshev(c) == 2));

        // The map travels with every candidate.
        assert!(cells.iter().all(|c| c.map_id == at.map_id));
    }

    /// Non-pileable items (equipment: weapons, armour, accessories) never merge,
    /// no matter which container they sit in. `stackable` is false for them, so
    /// a same-unique move swaps and a pickup lands in a free carry slot instead
    /// of piling two e.g. rings on top of each other.
    #[test]
    fn equipment_never_stacks_even_inside_bag_slots() {
        // Equipment does not stack, whether equipped or in a bag.
        assert!(!stackable(1)); // a sword
        assert!(!stackable(600)); // armour
        assert!(!stackable(7500)); // a ring
        // Pileable consumables and materials still merge.
        assert!(stackable(2000)); // a heal item
        assert!(stackable(4000)); // a book
        // Unknown uniques are not pileable, like the legacy's type-0 result.
        assert!(!stackable(0));
        assert!(!stackable(i32::from(u16::MAX)));
    }
}
