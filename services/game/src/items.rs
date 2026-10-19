//! Items: the bag and ground-item family — the 303/305 slot move, the 306
//! split, the 307 inventory drop, the 308 pickup, and the 7002 personal-shop
//! data reply.

use gz_cipher::Cipher;
use gz_db::{
    DropLocation, DropStatus, GroundClaim, ItemRepository, MoveOutcome, PgItemRepository,
    PickupStatus, SplitStatus,
};
use gz_proto::item::{
    Item, MAX_ITEM_PER_PERSON, MAX_ITEM_UNIQUE, decode_inv_cancellationsealingup, decode_inv_drop,
    decode_inv_pickup, decode_move_item, decode_split_item, encode_person_item_info,
};
use gz_proto::opcodes_game::GameMsg;
use gz_proto::personalshop::{ShopTradeData, decode_personal_shop_request, shop_mode_for_request};
use tokio::net::TcpStream;

use crate::{AnyError, GameSession, logln, send_encoded, send_raw};

/// Applies `GAMEMSG_REQUEST_ITEMMOVE` (303) and `GAMEMSG_REQUEST_ITEMCHANGE`
/// (305) — a drag between two slots of the character's 80-entry item array.
///
/// Both payloads are the same `cltMoveItemInfo { uiFrom, uiTo }` and the
/// legacy routes both to the same DB move path; the two opcodes are just the
/// client's way to say "this move landed on an empty slot" versus "...stacked
/// or overwrote an occupied slot".
///
/// The client applies the move locally by the time it sends
/// (`MouseOperation.cpp:2247-2249`), but the server still answers the two
/// affected slots with `GAMEMSG_RESPONSE_PERSONITEMINFO` (7210). The legacy's
/// opcode table has no `RESPONSE_ITEMMOVE`; instead the DB-server's move
/// response writes *both* slots back out (`ServerDB-Item.cpp:84-102`). The
/// local apply and this reply agree in the common case — the client already
/// swapped — but the merge case is exactly where the client cannot render the
/// result on its own: it applies a swap to two same-item stacks where the
/// server has merged them, so without the correcting 7210 pair the bag shows
/// two stacks against one in the database.
///
/// # Errors
///
/// [`gz_db::RepositoryError`] from the move, or a short body.
pub(crate) async fn handle_item_move(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    items: &PgItemRepository,
    character_id: i32,
    payload: &[u8],
) -> Result<(), AnyError> {
    let (from, to) = decode_move_item(payload)?;
    let outcome = items.move_slot(character_id, from, to).await?;
    let (detail, slots) = match outcome {
        MoveOutcome::SourceEmpty => ("source slot was empty".to_string(), None),
        MoveOutcome::Swapped {
            from: new_from,
            to: new_to,
        } => ("swapped".to_string(), Some((new_from, new_to))),
        MoveOutcome::Stacked {
            total,
            to: new_to,
            from: new_from,
        } => (
            format!("stacked, destination now holds {total}"),
            Some((new_from, new_to)),
        ),
    };
    logln!("game: itemmove {from} -> {to} for character {character_id}: {detail}",);

    // SourceEmpty moves nothing; every other outcome rewrites both slots back
    // out so the client's bag agrees with the store — most importantly the
    // merge, whose local result the client cannot produce itself.
    if let Some((new_from, new_to)) = slots {
        send_encoded(
            stream,
            outbound,
            GameMsg::ResponsePersoniteminfo,
            &encode_person_item_info(from, &new_from),
        )
        .await?;
        send_encoded(
            stream,
            outbound,
            GameMsg::ResponsePersoniteminfo,
            &encode_person_item_info(to, &new_to),
        )
        .await?;
    }
    Ok(())
}

/// Applies `GAMEMSG_REQUEST_ITEMSPLIT` (341): moves a chosen count out of one
/// stack into another slot.
///
/// Split and move share a shape because the legacy splits through the same
/// `sDBRequest_MoveItem` DB path (`Server-Item.cpp:4400-4406`): a split is a
/// move whose source record carries only the split count, and its reply is the
/// same `DBMSG_RESPONSE_ITEMMOVE` → two 7210 slot rewrites as an itemmove.
pub(crate) async fn handle_item_split(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    items: &PgItemRepository,
    character_id: i32,
    payload: &[u8],
) -> Result<(), AnyError> {
    let (from, to, count) = decode_split_item(payload)?;
    let outcome = items.split_slot(character_id, from, to, count).await?;
    let detail = match outcome.status {
        SplitStatus::Split => format!("split {count}"),
        SplitStatus::SourceEmpty => "source slot was empty".to_string(),
        SplitStatus::BadCount { stored, requested } => {
            format!("source holds {stored}, cannot split {requested}")
        }
        SplitStatus::DestinationMismatch { stored, claimed } => {
            format!("destination holds item {stored}, source is {claimed}")
        }
        SplitStatus::SelfSplit => "from == to, nothing to split".to_string(),
    };
    logln!("game: itemsplit {from} -> {to} x{count} for character {character_id}: {detail}");

    // Only a real split changes the bag; refusals are logged, matching the
    // itemmove rules — the client asked for a visible change, and a no-op
    // leaves its local state exactly as it was.
    if outcome.status == SplitStatus::Split {
        send_encoded(
            stream,
            outbound,
            GameMsg::ResponsePersoniteminfo,
            &encode_person_item_info(from, &outcome.from),
        )
        .await?;
        send_encoded(
            stream,
            outbound,
            GameMsg::ResponsePersoniteminfo,
            &encode_person_item_info(to, &outcome.to),
        )
        .await?;
    }
    Ok(())
}

/// Applies `GAMEMSG_REQUEST_INVDROP` (307) — dropping an item onto the map.
///
/// # Which frame comes back, and why it is 7210 and not 309
///
/// The opcode names invite the wrong answer. `GAMEMSG_RESPONSE_ITEMDROP` (309)
/// carries a `cltCharPos` plus the item's unique and list index, and the client's
/// handler does exactly one thing with it: `SetItemMap(x, y, …)` to draw a ground
/// icon (`Client-SystemNPC.cpp:2274`). It never touches the inventory.
///
/// The frame that updates the inventory is `GAMEMSG_RESPONSE_PERSONITEMINFO`
/// (7210), whose handler writes `clCharItem.clItem[siItemPos].Set(&clItem)` and
/// refreshes every open inventory dialog (`Client-Person-BigInfo.cpp:616`). The
/// client never optimistically updates its bag on a drop — `MouseOperation.cpp`
/// sends 307 and does nothing else — so without 7210 the slot simply never
/// changes. That is the whole of the missing confirmation.
///
/// Tracing the legacy confirms it: `DoMsg_GAMEMSG_REQUEST_INVDROP` ends by handing
/// the drop to the DB layer, and `DoMsg_DBMSG_RESPONSE_DROPITEM` answers the
/// client with 7210 carrying the slot's new contents (`ServerDB-Item.cpp:2833`).
/// The same 7210 is what the 307 handler sends when the client's record does not
/// match the server's (`Server-Item.cpp:2470`), which is why one reply covers
/// every outcome.
///
/// ## The 309 that makes it visible
///
/// The drop also goes out as `GAMEMSG_RESPONSE_ITEMDROP` (309), which is the only
/// thing that puts an icon on the ground. The legacy's ordering is
/// `DropItem` then 7210 (`DoMsg_DBMSG_RESPONSE_DROPITEM`, `ServerDB-Item.cpp:2792`)
/// with the 309 following from the cell notification; the client treats the two
/// independently — one writes the map grid, the other the bag — so the order here
/// is not load-bearing, and 7210 goes first to keep the reply path unchanged.
///
/// Contrary to what the map-layer framing suggests, 309 needs nothing from us but
/// the coordinates: `cltMapCommon::SetItemMap` (`map.cpp:289`) writes the client's
/// own per-cell grid, so the client supplies the map. What it *does* demand is that
/// `clPos.siMapIndex` equal the client's `CurrentMapIndex`, which it took from
/// `RESPONSE_LOGIN` — hence [`gz_proto::game::client_map_index`] on both sides.
///
/// The frame carries no stack count, so a 16-stack drop will render as one icon.
pub(crate) async fn handle_inv_drop(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    items: &PgItemRepository,
    character_id: i32,
    cell: DropLocation,
    payload: &[u8],
) -> Result<(), AnyError> {
    let request = decode_inv_drop(payload)?;

    // `Server-Item.cpp:2462` drops out of range positions silently.
    if request.slot < 0 || request.slot >= MAX_ITEM_PER_PERSON as i32 {
        logln!(
            "game: ignoring invdrop of item {} from slot {} (outside 0..{MAX_ITEM_PER_PERSON})",
            request.item.unique(),
            request.slot
        );
        return Ok(());
    }
    let slot = request.slot as u16;

    let outcome = items
        .drop_slot(character_id, slot, &request.item, cell)
        .await?;

    let detail = match outcome.status {
        DropStatus::Dropped => {
            let dropped = outcome.dropped.unwrap_or_else(Item::empty);
            match &outcome.grounded {
                Some(g) => format!(
                    "dropped item {} x{} -> map {} at ({}, {})",
                    dropped.unique(),
                    dropped.count(),
                    cell.map_id,
                    g.x,
                    g.y
                ),
                None => format!(
                    "dropped item {} x{} -> map {} at ({}, {})",
                    dropped.unique(),
                    dropped.count(),
                    cell.map_id,
                    cell.x,
                    cell.y
                ),
            }
        }
        DropStatus::SlotEmpty => format!("slot {slot} was empty"),
        DropStatus::ItemMismatch { stored, claimed } => {
            format!("slot {slot} holds item {stored}, not {claimed}")
        }
        DropStatus::NotEnough { stored, requested } => {
            format!("slot {slot} holds {stored}, cannot drop {requested}")
        }
        DropStatus::GroundBlocked { x, y } => {
            format!("drop cell ({x}, {y}): every cell of the 5x5 box around it is occupied")
        }
        DropStatus::GroundFull => {
            // Not a bug at shard scale (65,535 live ground items) but a hard
            // ceiling the pool has to name when it is hit: the drop was
            // refused, the slot untouched, and 7210 above already rolled the
            // client's optimistic view back — the session stays up.
            "ground handle space exhausted (65,535 live ground items), drop refused".to_owned()
        }
    };
    logln!("game: invdrop slot {slot} for character {character_id}: {detail}");

    // Every outcome is answered: the client needs its slot overwritten either to
    // show the removal or to undo an optimistic one it should not have made.
    send_encoded(
        stream,
        outbound,
        GameMsg::ResponsePersoniteminfo,
        &encode_person_item_info(slot, &outcome.current),
    )
    .await?;

    // Only a real drop produces a ground object to draw. The rejection paths leave
    // `grounded` as `None` precisely so nothing is drawn for them.
    if let Some(grounded) = outcome.grounded {
        send_encoded(
            stream,
            outbound,
            GameMsg::ResponseItemdrop,
            &grounded.encode(),
        )
        .await?;
        // The client discards this frame without a word if the cell is outside its
        // map, and we hold no map model to check against — so the log is the only
        // way to tell "drew" from "silently ignored".
        logln!(
            "game: sent itemdrop 309: map {} ({}, {}) unique {} handle {}",
            grounded.map_index,
            grounded.x,
            grounded.y,
            grounded.item_unique,
            grounded.list_index
        );
    }
    Ok(())
}

/// Answers `GAMEMSG_REQUEST_INVPICKUP` (308): a click on a ground item.
///
/// This is the other half of [`handle_inv_drop`]. The client sends five `SI16`
/// describing the cell it clicked (`MouseOperation.cpp:5316` passes
/// `GetItemUnique(), GetItemIndex(), CurrentMapIndex, GetItemX(), GetItemY()`
/// straight out of the pair that 309 stored), and the server either moves the
/// item into a free carry slot or does nothing.
///
/// ## Silence is the rejection
///
/// Unlike 307 — which is always acknowledged, because the client made an
/// optimistic change to its bag that has to be undone — 308 gets **no reply at
/// all** when it is refused. `DoMsg_GAMEMSG_REQUEST_INVPICKUP` returns before
/// reaching any send on all nine rejection paths (`Server-Item.cpp:2358-2392`).
/// That is what makes a stale click harmless: the client re-sends the same
/// request until something visibly changes, and a correction frame would fight
/// that. Every refusal here is therefore logged rather than answered, because
/// unlike 307 there is no client-visible symptom to correlate it with.
///
/// The two range checks the legacy does before touching the item list are kept on
/// the same side as in the original, so a request outside them never reaches the
/// database.
pub(crate) async fn handle_inv_pickup(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    items: &PgItemRepository,
    character_id: i32,
    payload: &[u8],
) -> Result<(), AnyError> {
    let request = decode_inv_pickup(payload)?;

    // `Server-Item.cpp:2358`: the unique is range-checked against MAX_ITEM_UNIQUE,
    // and `siItemIndex` must not be negative. Both are the client's own words
    // about a handle we allocated inside `1..=65535`, so in practice they always
    // pass -- which is exactly why they are worth checking rather than assuming.
    if i32::from(request.item_unique) <= 0
        || i32::from(request.item_unique) >= MAX_ITEM_UNIQUE
        || request.item_index < 0
    {
        logln!(
            "game: ignoring invpickup of unique {} at handle {} (outside the legacy's range)",
            request.item_unique,
            request.item_index
        );
        return Ok(());
    }

    let outcome = items
        .pickup_ground(
            character_id,
            GroundClaim {
                handle: request.item_index,
                item_unique: request.item_unique,
                map_index: request.map_index,
                x: request.x,
                y: request.y,
            },
        )
        .await?;

    let (slot, item) = match outcome.status {
        PickupStatus::PickedUp => match (outcome.slot, outcome.item) {
            (Some(slot), Some(item)) => (slot, item),
            // The repository builds both together on this path; a missing one is a
            // bug in it rather than a state we can meaningfully answer.
            _ => {
                logln!("game: invpickup reported success without a slot or item");
                return Ok(());
            }
        },
        PickupStatus::NoSuchItem { handle } => {
            logln!("game: invpickup: no ground item has handle {handle}");
            return Ok(());
        }
        PickupStatus::ItemMismatch { stored, claimed } => {
            logln!("game: invpickup: handle names unique {stored}, not {claimed}");
            return Ok(());
        }
        PickupStatus::CellMismatch { stored, claimed } => {
            logln!(
                "game: invpickup: item is at ({}, {}, {}), client said ({}, {}, {})",
                stored.0,
                stored.1,
                stored.2,
                claimed.0,
                claimed.1,
                claimed.2
            );
            return Ok(());
        }
        PickupStatus::NoFreeSlot => {
            logln!("game: invpickup: every carry slot is occupied");
            return Ok(());
        }
    };

    logln!(
        "game: picked up item {} x{} into slot {slot} for character {character_id}",
        item.unique(),
        item.count()
    );

    // The bag slot first: the client needs `clCharItem.clItem[siItemPos]` updated
    // before the icon leaves the ground, or it would briefly hold an item it has
    // no record of.
    send_encoded(
        stream,
        outbound,
        GameMsg::ResponsePersoniteminfo,
        &encode_person_item_info(slot, &item),
    )
    .await?;

    // Then the 309 that clears the cell. `GroundItem::removal` is the legacy's
    // own convention -- the item's coordinates with both handles zeroed
    // (`ServerMap.cpp:288`) -- so the same frame shape does both jobs.
    if let Some(cleared) = outcome.cleared {
        send_encoded(
            stream,
            outbound,
            GameMsg::ResponseItemdrop,
            &cleared.encode(),
        )
        .await?;
        logln!(
            "game: sent itemdrop 309 removal for map {} ({}, {})",
            cleared.map_index,
            cleared.x,
            cleared.y
        );
    }

    Ok(())
}

/// Answers `GAMEMSG_REQUEST_PERSONALSHOPDATA` (7002) with
/// `GAMEMSG_RESPONSE_PERSONALSHOPDATA` (7003), which is what makes the client
/// create the personal-shop or manufacture dialog (`Client-PersonalShop.cpp:87-88`
/// and `:127-128`).
///
/// The legacy handler is `Server-PersonalShop.cpp:41-101`: it resolves the
/// target by `siCharUnique` (`:45`), and for a shop that is not open fills
/// `clShopData` from the character — mode from the request's flags (`:54-68`),
/// person id (`:70`) and account name (`:71`) — then sends the whole
/// `cltShopTradeData` back (`:97-99`).
///
/// This server knows exactly one character: the session's own. A request naming
/// any other `siCharUnique` has no data behind it, so it is logged rather than
/// answered with someone else's identity. In a world with no other avatars the
/// client can only ever name its own character anyway — the send site passes the
/// right-clicked character's id (`NRightMouseMenuDlg.cpp:349`), and the only
/// avatar it has is itself.
///
/// # Errors
///
/// A malformed body ([`gz_proto::wire::WireError`]) or a socket/cipher failure.
pub(crate) async fn handle_personal_shop_data(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    session: &GameSession,
    char_unique: i32,
    payload: &[u8],
) -> Result<(), AnyError> {
    let request = decode_personal_shop_request(payload)?;

    let Some(character_id) = session.character_id else {
        logln!(
            "game: personal shop request for char_unique {} before a character was selected; \
             not answering",
            request.char_unique
        );
        return Ok(());
    };
    let own_unique = i16::try_from(char_unique).map_err(|_| {
        format!("session char_unique {char_unique} does not fit the SI16 siCharUnique")
    })?;
    if request.char_unique != own_unique {
        logln!(
            "game: personal shop request for char_unique {} but this session is {own_unique}; \
             another character's shop is not modelled yet, not answering",
            request.char_unique
        );
        return Ok(());
    }

    let shop = ShopTradeData {
        shop_mode: shop_mode_for_request(request.manufacture, request.maker),
        person_id: character_id,
        account_id: session.username.clone(),
    };
    let body = shop.encode_response(request.char_unique);
    send_raw(
        stream,
        outbound,
        GameMsg::ResponsePersonalshopdata as u16,
        &body,
    )
    .await?;
    logln!(
        "game: personal shop reply for char_unique {}: mode {}, person_id {}, account {:?} \
         ({} bytes)",
        request.char_unique,
        shop.shop_mode,
        shop.person_id,
        shop.account_id,
        body.len()
    );
    Ok(())
}

/// What a `GAMEMSG_REQUEST_INV_CANCELLATIONSEALINGUP` (7223) should do to the slot.
///
/// Pure, so the decision is testable without a database or a socket — the same
/// split the quest family uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnsealPlan {
    /// Stamp this date into the record and store it.
    Unseal(Item),
    /// Nothing to write, and the reason for the log.
    NoChange(&'static str),
}

/// The decision behind 7223: given the slot's stored record and today's date, what
/// should be written?
///
/// This is `DoMsg_GAMEMSG_REQUEST_INV_CANCELLATIONSEALINGUP`'s core
/// (`Server-Item.cpp:3781-3795`): read the live array, and if the record is sealed
/// stamp today's date into it. The legacy guards that this port does not model the
/// same way are the personal-shop checks (no player shops yet) and the
/// `ITEMATB_OBJECT` / level-under-30 / crystal exclusions inside `IsSealingUp` — the
/// item table this port carries has no attribute bits, and the client only offers
/// the unseal action for items its own `IsSealingUp` accepts, so the stored date trio
/// is the whole test here. Documented on the card.
#[must_use]
pub(crate) fn unseal_plan(stored: &Item, today: (u8, u8, u8)) -> UnsealPlan {
    if stored.is_empty() {
        return UnsealPlan::NoChange("slot is empty");
    }
    if !stored.is_sealed() {
        return UnsealPlan::NoChange("item is not sealed");
    }
    let mut item = *stored;
    item.set_seal_date(today.0, today.1, today.2);
    UnsealPlan::Unseal(item)
}

/// `GAMEMSG_REQUEST_INV_CANCELLATIONSEALINGUP` (7223): the player asked to unseal a
/// bag slot.
///
/// A sealed item is one whose `uiDateYear`/`uiDateMonth`/`uiDateDay` are all zero
/// (`ItemUnit.cpp:3463-3467`), which is how a freshly granted record starts — so the
/// shoes a quest hands out arrive sealed and take this request to become equippable.
///
/// The store, not the client, decides: the request carries the client's copy of the
/// record, and it is decoded but never trusted (`Server-Item.cpp:3781` reads the live
/// array by slot).
///
/// The answer is `GAMEMSG_RESPONSE_PERSONITEMINFO` (7210) with the slot's new record
/// — the legacy's own reply (`Server-Item.cpp:3868-3873`). There is *no* 7224
/// response frame on this path, and the shipped client has no handler for one.
///
/// # Errors
///
/// A short body is logged and ignored, matching the family's soft-fail rule;
/// repository and socket failures propagate.
pub(crate) async fn handle_inv_cancellation_sealing_up(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    items: &PgItemRepository,
    character_id: i32,
    today: (u8, u8, u8),
    payload: &[u8],
) -> Result<(), AnyError> {
    let Ok(request) = decode_inv_cancellationsealingup(payload) else {
        logln!("game: malformed 7223 ({} bytes) — ignored", payload.len());
        return Ok(());
    };
    let Ok(slot) = u16::try_from(request.slot) else {
        logln!("game: unseal for negative slot {} — ignored", request.slot);
        return Ok(());
    };
    if usize::from(slot) >= MAX_ITEM_PER_PERSON {
        logln!("game: unseal slot {slot} out of range — ignored");
        return Ok(());
    }

    let stored = items
        .list(character_id)
        .await?
        .into_iter()
        .find(|row| row.slot == slot);
    let Some(row) = stored else {
        logln!("game: unseal slot {slot} for character {character_id}: slot is empty");
        return Ok(());
    };
    let current = Item::decode(&row.raw)?;

    match unseal_plan(&current, today) {
        UnsealPlan::NoChange(why) => {
            logln!("game: unseal slot {slot} for character {character_id}: {why}");
        }
        UnsealPlan::Unseal(item) => {
            items.write_slot(character_id, slot, &item).await?;
            logln!(
                "game: unseal slot {slot} for character {character_id}: item {} dated {}-{}-{}",
                item.unique(),
                item.seal_date().0,
                item.seal_date().1,
                item.seal_date().2
            );
            send_encoded(
                stream,
                outbound,
                GameMsg::ResponsePersoniteminfo,
                &encode_person_item_info(slot, &item),
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record exactly as the store holds the quest shoes that started this: item
    /// 20164, one of it, every date byte zero (the live row for character 35, slot
    /// 16).
    fn sealed_shoes() -> Item {
        let mut raw = [0u8; gz_proto::item::CLT_ITEM_SIZE];
        raw[0..2].copy_from_slice(&20164u16.to_le_bytes());
        raw[2..4].copy_from_slice(&1u16.to_le_bytes());
        Item::decode(&raw).expect("a whole record")
    }

    #[test]
    fn a_sealed_item_is_dated_with_today() {
        let plan = unseal_plan(&sealed_shoes(), (26, 10, 13));
        let UnsealPlan::Unseal(item) = plan else {
            panic!("sealed item must unseal, got {plan:?}");
        };
        assert_eq!(item.seal_date(), (26, 10, 13));
        assert!(!item.is_sealed());
        // Nothing else moved: same unique, same count, same use-days.
        assert_eq!(item.unique(), 20164);
        assert_eq!(item.count(), 1);
        assert_eq!(item.use_days(), sealed_shoes().use_days());
    }

    /// The date trio is the whole seal test, and the offsets are pinned against a
    /// real record: item 705 in a dev character's slot reads `1a 09 17` at +4.
    #[test]
    fn the_seal_test_is_the_date_trio() {
        let mut raw = [0u8; gz_proto::item::CLT_ITEM_SIZE];
        raw[0..2].copy_from_slice(&705u16.to_le_bytes());
        raw[2..4].copy_from_slice(&1u16.to_le_bytes());
        raw[4] = 26;
        raw[5] = 9;
        raw[6] = 23;
        let dated = Item::decode(&raw).expect("a whole record");
        assert_eq!(dated.seal_date(), (26, 9, 23));
        assert!(!dated.is_sealed());
        assert!(matches!(
            unseal_plan(&dated, (26, 10, 13)),
            UnsealPlan::NoChange(_)
        ));
    }

    #[test]
    fn an_empty_slot_and_a_stamped_item_are_both_refused() {
        assert!(matches!(
            unseal_plan(&Item::empty(), (26, 10, 13)),
            UnsealPlan::NoChange("slot is empty")
        ));
        let mut stamped = sealed_shoes();
        stamped.set_seal_date(26, 10, 1);
        assert!(matches!(
            unseal_plan(&stamped, (26, 10, 13)),
            UnsealPlan::NoChange("item is not sealed")
        ));
    }

    /// A 7223 body is the 88 bytes the client sent: the slot, then its copy of the
    /// record.
    #[test]
    fn a_7223_body_decodes_to_the_slot_and_the_clients_copy() {
        let mut body = Vec::new();
        body.extend_from_slice(&16i32.to_le_bytes());
        body.extend_from_slice(&sealed_shoes().encode());
        assert_eq!(
            body.len(),
            gz_proto::item::REQUEST_INV_CANCELLATIONSEALINGUP_SIZE
        );
        let decoded = decode_inv_cancellationsealingup(&body).expect("88 bytes");
        assert_eq!(decoded.slot, 16);
        assert_eq!(decoded.item.unique(), 20164);
        assert!(decoded.item.is_sealed());
    }
}
