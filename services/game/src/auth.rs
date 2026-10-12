//! Auth and character lifecycle: the char-select admission rule, the 15-slot
//! roster (opcode 3), game login (1), selectchar (2), make-new-char (7) and the
//! 9/83 delete / pre-delete pair.

use gz_cipher::Cipher;
use gz_db::{
    AccountRepository, CharacterFull, CharacterRepository, PgAccountRepository,
    PgCharacterRepository, RepositoryError, password,
};
use gz_proto::game::{
    CharListEntry, GameLogin, MakeCharInfo, client_map_index, decode_make_new_char,
    encode_del_result,
};
use gz_proto::opcodes_game::GameMsg;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Notify;

use crate::config::GameConfig;
use crate::registry::{Registry, SessionKey};
use crate::{AnyError, CHAR_SLOTS, GameSession, logln, send_encoded, server_info};
/// Whether a `GAMEMSG_REQUEST_SELECTCHAR` should be honoured on this connection.
///
/// The old reading here ("world entry is one-shot, a repeat is a silent no-op in
/// every case the legacy can reach") was **wrong** and is corrected here. The
/// legacy's guard is the *bound character*, not the act of having entered:
///
/// 1. `Server-Login.cpp:184` reads `idnum` from `pclCI->clBI.siIDNum`, which is
///    the **account** idnum, not the character's (`LiveFrames` relies on the same
///    fact for 248). `uiIndex` is the requested slot.
/// 2. `rsp_PersonIDGet` (`Goonzu_DB\04. GameDB___________.txt:1073`) selects
///    `WHERE [idnum] = @idnum and [index] = @index`. With `idnum` the account, a
///    request for *any* slot of the account matches a row.
/// 3. `ServerDB-Person.cpp:1757` returns only when the DB result is empty (no
///    such slot). For an occupied slot, `:1763` returns only when the requested
///    character is the one already bound (`GetPersonID()` equals the request's).
///    Otherwise `:1770` runs `CloseAction`, `:1795` `DeleteChar`s the old avatar,
///    and the new character is announced through `SendServerInfo`.
///
/// So a second selectchar naming a **different** character is legal and must
/// tear down and reload; only re-selecting the *same* character is a no-op. The
/// client relies on this: the exit dialog's character-select button sets
/// `GS_INITCHARLIST` with no server message (`NExitDlg.cpp:288`), reusing the
/// same connection, and `CNSelectCharDlg::SelectChar()` then sends the request
/// (`NSelectChar.cpp:1287`).
pub(crate) fn admit_select_char(bound_slot: Option<i16>, slot: i32) -> bool {
    match bound_slot {
        // No character entered yet: the first selectchar always proceeds.
        None => true,
        // Same slot: the legacy detects the same PersonID and returns.
        Some(bound) => i32::from(bound) != slot,
    }
}

/// Whether a make-new-char `slot` resolves to one of the 15 roster rows.
///
/// The client only ever sends 0..CHAR_SLOTS, but a crafted `slot >= CHAR_SLOTS`
/// would persist a character the roster never lists (see `send_char_list`, which
/// walks exactly `0..CHAR_SLOTS`): an invisible and unloseable row.
pub(crate) fn admit_make_new_char_slot(slot: u8) -> bool {
    (0..CHAR_SLOTS).contains(&i16::from(slot))
}

/// Sends every slot's `cltCharListInfo` (opcode 3), zeroed for empty slots, so
/// the client's `PutCharList` writes 15 copies and the roster dialog repaints.
pub(crate) async fn send_char_list(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
    account_id: i32,
    account_name: &str,
) -> Result<usize, AnyError> {
    let rows = characters.list_for_account(account_id).await?;
    let mut occupied = 0usize;
    for slot in 0..CHAR_SLOTS {
        let character = rows.iter().find(|c| c.slot == slot);
        if character.is_some() {
            occupied += 1;
        }
        // Empty slots are sent too, zeroed except for the index: the client's
        // `PutCharList` copies each packet into `clCharListInfo[slot]`
        // unconditionally (`Client.cpp:4591-4599`).
        let entry = match character {
            Some(c) => CharListEntry {
                account_idnum: account_id,
                account_id: account_name.to_owned(),
                slot: slot as u8,
                sex: (c.gender != 0) as u8,
                class_kind: c.class_kind as u16,
                character_id: c.character_id,
                name: c.name.clone(),
                level: c.level,
                life: c.max_life,
                mana: c.max_mana,
                exp: c.exp,
            },
            None => CharListEntry {
                account_idnum: 0,
                account_id: String::new(),
                slot: slot as u8,
                sex: 0,
                class_kind: 0,
                character_id: 0,
                name: String::new(),
                level: 0,
                life: 0,
                mana: 0,
                exp: 0,
            },
        };
        send_encoded(stream, outbound, GameMsg::ResponseCharlist, &entry.encode()).await?;
    }
    Ok(occupied)
}

/// The admission decision for `GAMEMSG_REQUEST_LOGIN` (opcode 1).
///
/// Kept separate from the repository read so it can be tested without a
/// database. Two independent checks, both required:
///
/// * `auth_key` must equal the account's own idnum. The front hands the
///   account id out as the key, so a mismatch is a stale or replayed key.
/// * the password carried in the packet must verify against the stored hash.
///   The client is told the game address by the front and connects directly,
///   so without this check any client could name a userid and its guessable
///   `SERIAL` account id and enter without ever passing the front's login.
///   The legacy validates the same pair through the auth server
///   (`Server-Login.cpp:82` forwards `siAuthKey` and `szPassword`).
///
/// # Errors
///
/// Names which of the two checks failed, for the log.
pub(crate) fn admit_game_login(
    account_id: i32,
    password_hash: &str,
    login: &GameLogin,
) -> Result<(), String> {
    if account_id != login.auth_key {
        return Err(format!(
            "game login auth key {} does not match account {account_id}",
            login.auth_key
        ));
    }
    if !password::verify_password(password_hash, &login.password) {
        return Err(format!(
            "game login for {:?}: password does not match",
            login.userid
        ));
    }
    Ok(())
}

/// S9–S11: verify the game login and answer with one char-list packet per slot.
///
/// Before the char list is served, the account is claimed for this connection
/// ([`Registry::claim_login`]). A second login for an account already logged in
/// is refused with an error, which the caller turns into a disconnect, **and**
/// the holder's connection is woken to disconnect — the legacy closes both
/// connections on a duplicate (`Server-Auth.cpp:51-120`). Both halves live in
/// `claim_login`: the refusal is the returned `Err`, the kick is the holder's
/// `close` being notified.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_game_login(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    accounts: &PgAccountRepository,
    characters: &PgCharacterRepository,
    registry: &Registry,
    session_key: SessionKey,
    close: Arc<Notify>,
    login: &GameLogin,
) -> Result<GameSession, AnyError> {
    // The auth key is the account idnum the front handed out at login, and the
    // password must match the stored hash. A mismatch on either means a stale,
    // replayed or forged login, so refuse rather than guess.
    let account = match accounts.find_by_username(&login.userid).await {
        Ok(account) => account,
        Err(RepositoryError::NotFound) => {
            return Err(format!("game login for unknown account {:?}", login.userid).into());
        }
        Err(e) => return Err(e.into()),
    };
    admit_game_login(account.account_id, &account.password_hash, login).map_err(AnyError::from)?;

    // One live login per account. A conflict refuses *this* connection (the
    // caller drops the socket on any `Err`) and wakes the holder to close itself.
    if let Some(owner) = registry.claim_login(account.account_id, session_key, close) {
        return Err(format!(
            "game login for {:?}: account {} is already logged in on session {owner}; \
             refusing the duplicate login and closing the holder",
            login.userid, account.account_id
        )
        .into());
    }

    let occupied = send_char_list(
        stream,
        outbound,
        characters,
        account.account_id,
        &account.username,
    )
    .await?;

    logln!(
        "game: login ok for {:?} (account {}, auth_key {}): {occupied} character(s)",
        login.userid,
        account.account_id,
        login.auth_key
    );
    Ok(GameSession {
        account_id: account.account_id,
        gm_level: account.gm_level,
        character_id: None,
        username: account.username,
        entry_cell: None,
    })
}

/// S12/S13: world entry. Answers `GAMEMSG_REQUEST_SELECTCHAR` with the
/// character record, matching the legacy order in `ServerDB-Person.cpp`: the
/// server info first, then `GAMEMSG_RESPONSE_LOGIN` carrying `clMakeCharInfo`.
///
/// The client only accepts slots `0..3` at select time
/// (`Server-Login.cpp:187`).
pub(crate) async fn handle_select_char(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
    cfg: &GameConfig,
    session: &GameSession,
    char_unique: i32,
    slot: i32,
) -> Result<CharacterFull, AnyError> {
    if !(0..i32::from(CHAR_SLOTS)).contains(&slot) {
        return Err(format!("selectchar slot {slot} out of range 0..{CHAR_SLOTS}").into());
    }

    let character = characters
        .find_by_slot(session.account_id, slot as i16)
        .await?
        .ok_or_else(|| format!("no character in account {} slot {slot}", session.account_id))?;

    // The character now exists as a world entity, so give it a durable id if it
    // has never had one. It is *not* what goes on the wire: `make_char_unique`
    // below echoes the session's `char_unique`, and every replayed frame carries
    // that same session id, so the wire is self-consistent either way.
    //
    // The mismatch is therefore *inert today* and only worth reporting as future
    // debt: once T-017 generates per-character frames from the database instead
    // of replaying, those frames will carry `stored_unique`, and a session
    // holding a different `char_unique` will have them all discarded by
    // `SetCharReservedChar`.
    //
    // The earlier wording of this message ("the replayed per-character frames
    // will be discarded by the client") was wrong about the mechanism and
    // misled a 2026-10-05 diagnosis into blaming the id mismatch for a
    // character-switch desync that was a missing already-bound guard instead.
    // A character whose id *does* match, `Tester`, glitched identically; and
    // `test3`, which does not match, entered the world cleanly.
    let stored_unique = characters
        .resolve_game_unique(character.character_id)
        .await?;
    if stored_unique != char_unique {
        logln!(
            "game: selectchar {:?} has durable game_unique {stored_unique} but this session's \
             char_unique is {char_unique}; harmless while the world is replayed (every frame \
             carries the session id), but T-017 must make these agree or the client will discard \
             this character's records",
            character.name
        );
    }

    // SERVERINFO sets `bReceiveServerInfoSwitch`, which `SetCharReservedChar`
    // checks before it will create any avatar. It must precede RESPONSE_LOGIN.
    send_encoded(
        stream,
        outbound,
        GameMsg::ResponseServerinfo,
        &server_info(cfg).encode(),
    )
    .await?;

    let make_char = MakeCharInfo {
        // Must equal the `char_unique` from opcode 1 or the client ignores the
        // record (`SetCharReservedChar` compares `MyCharUnique`).
        make_char_unique: u16::try_from(char_unique).unwrap_or(0),
        make_char_kind: u16::try_from(character.class_kind.max(0)).unwrap_or(0),
        level: u8::try_from(character.level.clamp(0, 255)).unwrap_or(1),
        life: u32::try_from(character.max_life.max(0)).unwrap_or(0),
        x: u16::try_from(character.pos_x.clamp(0, 0x3FF)).unwrap_or(0),
        y: u16::try_from(character.pos_y.clamp(0, 0x3FF)).unwrap_or(0),
        // Shared with the 309 encoder: the client copies this straight into
        // `CurrentMapIndex` and then drops any 309 naming a different map.
        map_index: client_map_index(character.map_id),
        // The character's effective `GetTotalMoveSpeed()` (base class speed plus
        // gear; `character_stats.move_speed`), not a fixed reference value.
        total_move_speed: u8::try_from(character.move_speed.clamp(0, 63)).unwrap_or(0),
        home_village_unique: u8::try_from(character.school_id.clamp(0, 255)).unwrap_or(0),
        game_master_level: u8::try_from(session.gm_level.clamp(0, 255)).unwrap_or(0),
        // 0 lets the client resolve the map type from its own MapData.
        map_unique: 0,
        make_ani_type: 0,
    };
    send_encoded(
        stream,
        outbound,
        GameMsg::ResponseLogin,
        &make_char.encode(),
    )
    .await?;

    logln!(
        "game: selectchar slot {slot} char {:?} -> map {} at ({}, {})",
        character.name,
        character.map_id,
        character.pos_x,
        character.pos_y
    );
    Ok(character)
}

/// Applies `GAMEMSG_REQUEST_MAKENEWCHAR` (7): persist a new character and
/// echo its `cltCharListInfo` with a success response (opcode 8).
pub(crate) async fn handle_make_new_char(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
    account_id: i32,
    account_name: &str,
    payload: &[u8],
) -> Result<(), AnyError> {
    let (name, sex, class_kind, slot) = decode_make_new_char(payload)?;
    if !admit_make_new_char_slot(slot) {
        return Err(format!("make-new-char slot {slot} out of range 0..{CHAR_SLOTS}").into());
    }
    let character_id = characters
        .create_character(
            account_id,
            &name,
            class_kind as i16,
            sex as i16,
            slot as i16,
        )
        .await?;
    let entry = CharListEntry {
        account_idnum: account_id,
        account_id: account_name.to_owned(),
        slot,
        sex,
        class_kind,
        character_id,
        name: name.clone(),
        level: 1,
        life: 40,
        mana: 20,
        exp: 0,
    };
    send_encoded(
        stream,
        outbound,
        GameMsg::ResponseMakenewchar,
        &entry.encode(),
    )
    .await?;
    logln!("game: created char {name:?} id={character_id} slot={slot} for account {account_id}");
    Ok(())
}

/// Applies `GAMEMSG_REQUEST_DELUSERCHAR` (9) and
/// `GAMEMSG_REQUEST_PREDELUSERCHAR` (83): the final "yes, delete" and the
/// grace-mark request. Both reach the same slot-clear response (opcodes 10/84).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_userchar_deletion(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
    account_id: i32,
    account_name: &str,
    response_opcode: GameMsg,
    hard_delete: bool,
    payload: &[u8],
) -> Result<(), AnyError> {
    let slot = payload.first().copied().unwrap_or(0) as i16;
    if !(0..CHAR_SLOTS).contains(&slot) {
        logln!("game: char-del slot {slot} out of range for account {account_id}");
        send_encoded(
            stream,
            outbound,
            response_opcode,
            &encode_del_result(0, 0, slot as i8),
        )
        .await?;
        return Ok(());
    }

    let affected = if hard_delete {
        characters.delete_character(account_id, slot).await?
    } else {
        // `GAMEMSG_REQUEST_PREDELUSERCHAR` body is `SI08 siCharSlot;
        // TCHAR szIdentification[12]; SI16 predeltype`. `predeltype = 1`
        // marks the char for grace removal, `0` cancels the pending grace.
        let deltype = if payload.len() > 13 {
            payload[13] as u16 | ((payload.get(14).copied().unwrap_or(0) as u16) << 8)
        } else {
            1
        };
        characters
            .set_delete_pending(account_id, slot, deltype != 0)
            .await?
    };

    match affected {
        Some(id) => {
            send_encoded(
                stream,
                outbound,
                response_opcode,
                &encode_del_result(1, 0, slot as i8),
            )
            .await?;
            logln!("game: char-del slot {slot} id {id} for account {account_id}: done");
            // The slot is now removed locally but the client's roster dialog
            // does not repaint until its existing cache is refreshed (no
            // notification flows with opcode 10/84). Re-emit all 15 char-list
            // packets so `PutCharList` rebuilds every slot and the dialog
            // repaints without a user click. This is a dev convenience:
            // the legacy never emitted opcode 3 outside of login.
            let _occupied =
                send_char_list(stream, outbound, characters, account_id, account_name).await?;
        }
        None => {
            send_encoded(
                stream,
                outbound,
                response_opcode,
                &encode_del_result(0, 0, slot as i8),
            )
            .await?;
            logln!("game: char-del slot {slot} for account {account_id}: slot already empty");
        }
    }
    Ok(())
}
