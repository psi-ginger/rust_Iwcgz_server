//! Seeds a character's `character_items` from the replayed reference capture.
//!
//! The world-entry burst this service replays contains `RESPONSE_ALLCHARINFOTWO`
//! (249), whose first 6720 bytes are the character's 80-slot item array. Those
//! bytes are somebody else's inventory — the reference session's character, not
//! ours — but they are also the only complete, self-consistent set of `cltItem`
//! records we have. Importing them turns the fixture into real rows: the
//! inventory the user can already see becomes ours, and the encoder written
//! later has a known-good expected output to diff against.
//!
//! Deliberately lossy in exactly one direction: an occupied slot is imported as
//! it stands. Slots whose only non-zero content is the `siUnique`/`siItemNum`
//! pair (several in the capture) are imported too — they are what the reference
//! server sent, and guessing at a rule for them would be worse than carrying them.

use gz_db::{CharacterItem, ItemRepository, PgItemRepository, container_of};
use gz_proto::item::{MAX_ITEM_PER_PERSON, decode_item_array};
use gz_proto::opcodes_game::GameMsg;

use crate::generated_capture;

/// What [`run`] did, for the caller's log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportReport {
    /// Slots the capture left empty.
    pub empty: usize,
    /// Slots with an item.
    pub occupied: usize,
    /// Slots carrying a unique and a count but no other record content.
    ///
    /// Not an error — just the count the caller should be suspicious of, because
    /// the meaning of those records has not been established yet.
    pub sparse: usize,
}

/// Replaces `character_id`'s item rows with the captured reference array.
///
/// # Errors
///
/// Fails if the capture has no `RESPONSE_ALLCHARINFOTWO` frame, if that frame is
/// too short to hold the item array, or if the write fails.
pub async fn run(
    items: &PgItemRepository,
    character_id: i32,
) -> Result<ImportReport, Box<dyn std::error::Error + Send + Sync>> {
    let body = generated_capture::frame(GameMsg::ResponseAllcharinfotwo as u16)
        .ok_or("the capture has no RESPONSE_ALLCHARINFOTWO (249) frame to import from")?;
    let decoded = decode_item_array(body)?;

    let mut report = ImportReport {
        empty: 0,
        occupied: 0,
        sparse: 0,
    };
    let mut rows = Vec::with_capacity(MAX_ITEM_PER_PERSON);
    for (slot, item) in decoded.iter().enumerate() {
        if item.is_empty() {
            report.empty += 1;
            continue;
        }
        report.occupied += 1;
        if item.raw[4..].iter().all(|&b| b == 0) {
            report.sparse += 1;
        }
        rows.push(CharacterItem {
            slot: slot as u16,
            container: container_of(slot as u16),
            item_unique: i32::from(item.unique()),
            count: i32::from(item.count()),
            raw: item.raw.to_vec(),
        });
    }

    items.replace_all(character_id, &rows).await?;
    Ok(report)
}
