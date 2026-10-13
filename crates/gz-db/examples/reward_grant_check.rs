//! Scratch live check for the quest-reward storage calls (T-019 reward work).
//!
//! Not a repo artifact: run once against the dev database, print what happened,
//! then delete. It exists because the placement rule and the money write are
//! unit-tested but the SQL behind them is not — this is the only layer the unit
//! tests cannot reach.
//!
//! Run with: `cargo run --example reward_grant_check -p gz-db -- [character_id]`
//! (`DATABASE_URL` must point at a live database, as `scripts\dev-db.ps1` sets up)
//!
//! It grants to the given character (default 21, "Tester2" on the dev box), prints
//! the rows it created, and restores the character by deleting exactly those rows
//! and subtracting the money again.

use gz_db::{CharacterRepository, ItemRepository, PgCharacterRepository, PgItemRepository};

const NON_PILEABLE: u16 = 1; // a sword
const PILEABLE: u16 = 2000; // a heal item
const MONEY: i64 = 100;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let character_id: i32 = std::env::args()
        .nth(1)
        .map(|arg| arg.parse().expect("character id"))
        .unwrap_or(21);
    let url = std::env::var("DATABASE_URL")
        .map_err(|_| "DATABASE_URL must be set (see scripts/dev-db.ps1)")?;
    let pool = sqlx::PgPool::connect(&url).await?;
    let items = PgItemRepository::connect(&url).await?;
    let characters = PgCharacterRepository::connect(&url).await?;

    let before = items.list(character_id).await?;
    let money_before: i64 =
        sqlx::query_scalar("SELECT money FROM characters WHERE character_id = $1")
            .bind(character_id)
            .fetch_one(&pool)
            .await?;
    println!(
        "before: {character_id}: {} occupied slot(s) {:?}, money {money_before}",
        before.len(),
        before.iter().map(|r| r.slot).collect::<Vec<_>>()
    );

    // 1. A non-pileable reward is one item, whatever the reward asked for.
    let a = items.grant(character_id, NON_PILEABLE, 5).await?;
    println!("grant sword x5 -> {a:?}");
    // 2. A pileable reward keeps its count.
    let b = items.grant(character_id, PILEABLE, 3).await?;
    println!("grant heal x3 -> {b:?}");
    // 3. …and a second grant tops the same stack up, reporting the new total.
    let c = items.grant(character_id, PILEABLE, 2).await?;
    println!("grant heal x2 again -> {c:?}");
    // 4. Money.
    characters.add_money(character_id, MONEY).await?;
    let money_after: i64 =
        sqlx::query_scalar("SELECT money FROM characters WHERE character_id = $1")
            .bind(character_id)
            .fetch_one(&pool)
            .await?;

    let after = items.list(character_id).await?;
    println!(
        "after:  {} occupied slot(s) {:?}, money {money_after}",
        after.len(),
        after
            .iter()
            .map(|r| (r.slot, r.item_unique, r.count))
            .collect::<Vec<_>>()
    );

    let mut failures = Vec::new();
    if money_after != money_before + MONEY {
        failures.push(format!("money {money_before} + {MONEY} != {money_after}"));
    }
    for (label, got, want) in [
        ("sword slot", a.as_ref().map(|r| i64::from(r.slot)), 15i64),
        ("sword count", a.as_ref().map(|r| i64::from(r.count)), 1),
        ("heal slot", b.as_ref().map(|r| i64::from(r.slot)), 16),
        ("heal count", b.as_ref().map(|r| i64::from(r.count)), 3),
        ("merged slot", c.as_ref().map(|r| i64::from(r.slot)), 16),
        ("merged count", c.as_ref().map(|r| i64::from(r.count)), 5),
    ] {
        if got != Some(want) {
            failures.push(format!("{label}: got {got:?}, want {want:?}"));
        }
    }
    // The stored record must carry the merged count, not the granted delta.
    if let Some(row) = &c {
        let item = gz_proto::item::Item::decode(&row.raw)?;
        if i32::from(item.count()) != 5 {
            failures.push(format!(
                "slot record count is {} after the merge, want 5",
                item.count()
            ));
        }
    }

    // Restore: drop exactly the two slots this run created and give the money back.
    for slot in [a.as_ref().map(|r| r.slot), b.as_ref().map(|r| r.slot)]
        .into_iter()
        .flatten()
    {
        sqlx::query("DELETE FROM character_items WHERE character_id = $1 AND slot = $2")
            .bind(character_id)
            .bind(i16::try_from(slot).unwrap_or(0))
            .execute(&pool)
            .await?;
    }
    characters.add_money(character_id, -MONEY).await?;
    let restored = items.list(character_id).await?;
    let money_restored: i64 =
        sqlx::query_scalar("SELECT money FROM characters WHERE character_id = $1")
            .bind(character_id)
            .fetch_one(&pool)
            .await?;
    println!(
        "restored: {} occupied slot(s), money {money_restored}",
        restored.len()
    );

    // --- write_slot: the single-record write the unseal path uses ---
    // On this character's (empty) slot 17, so nothing of anyone's is touched: write a
    // sealed record, read it back, stamp a date the way 7223 does, read it back again,
    // then delete the row.
    const WRITE_SLOT: u16 = 17;
    let mut sealed = gz_proto::item::Item::from_unique(20164, 1);
    if !sealed.is_sealed() {
        failures.push("a fresh record should be sealed".into());
    }
    let written = items.write_slot(character_id, WRITE_SLOT, &sealed).await?;
    let read_back = items
        .list(character_id)
        .await?
        .into_iter()
        .find(|r| r.slot == WRITE_SLOT);
    println!("write_slot sealed -> {written:?}");
    match read_back
        .as_ref()
        .map(|r| gz_proto::item::Item::decode(&r.raw))
    {
        Some(Ok(item)) if item == sealed => {}
        other => failures.push(format!("sealed record did not round-trip: {other:?}")),
    }

    sealed.set_seal_date(26, 10, 13);
    items.write_slot(character_id, WRITE_SLOT, &sealed).await?;
    let after = items
        .list(character_id)
        .await?
        .into_iter()
        .find(|r| r.slot == WRITE_SLOT)
        .map(|r| gz_proto::item::Item::decode(&r.raw));
    match after {
        Some(Ok(item))
            if item.seal_date() == (26, 10, 13) && !item.is_sealed() && item.unique() == 20164 =>
        {
            println!("write_slot unsealed -> date {:?}", item.seal_date());
        }
        other => failures.push(format!("unsealed record did not land: {other:?}")),
    }

    // An empty record clears the slot, which is what `write_slot` promises.
    let cleared = items
        .write_slot(character_id, WRITE_SLOT, &gz_proto::item::Item::empty())
        .await?;
    let left = items
        .list(character_id)
        .await?
        .into_iter()
        .filter(|r| r.slot == WRITE_SLOT)
        .count();
    println!("write_slot empty -> {cleared:?}, rows left at slot {WRITE_SLOT}: {left}");
    if cleared.is_some() || left != 0 {
        failures.push("an empty record must delete the row".into());
    }

    if failures.is_empty() {
        println!("\nRESULT: PASS");
        Ok(())
    } else {
        println!("\nRESULT: FAIL");
        for f in failures {
            println!("  - {f}");
        }
        Err("live grant check failed".into())
    }
}
