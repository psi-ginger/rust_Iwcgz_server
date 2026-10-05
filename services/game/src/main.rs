//! Game service — the second client-facing hop (blueprint 01 Part B, 05
//! §S8–S12).
//!
//! Reproduces the legacy `GoonZuWorld` server's login half without the
//! `AuthServer`/`GameDBManager` cluster: game login is verified straight against
//! PostgreSQL, and the character list comes from `tbl_Person`/`tbl_IP`.
//!
//! # Cipher pairing (this is the part that bites)
//!
//! The game link is **encoded in both directions, but with different keys**
//! (`char.cpp:5177-5219`, `GameProtocol.cpp:76`, `ITZSendBuffer.cpp:122-124`):
//!
//! - server → client uses `GetEncrypt2()`, the fixed link cipher (`0x12,0x34`);
//! - client → server uses `GetEncrypt()`, the session cipher keyed by the two
//!   bytes carried in `GAMEMSG_RESPONSE_CLIENTCHARUNIQUE`.
//!
//! So this service **encodes with [`Cipher::link_cipher`]** and **decodes with
//! [`Cipher::session_cipher`]** — the exact mirror of the front service, which
//! encodes with neither and decodes with the session cipher.
//!
//! Environment: `DATABASE_URL`, `GAME_LISTEN`.

use std::borrow::Cow;
use std::time::{SystemTime, UNIX_EPOCH};

use gz_cipher::Cipher;
use gz_db::DropStatus;
use gz_db::{
    AccountRepository, CharacterFull, CharacterRepository, DropLocation, GroundClaim,
    ItemRepository, MoveOutcome, PgAccountRepository, PgCharacterRepository, PgItemRepository,
    PickupStatus, RepositoryError, SplitStatus,
};
use gz_net::{COMPRESSION_FLAG, Frame, FrameParser, HEADER_SIZE, MAX_FRAME_SIZE};
use gz_proto::game::{
    CharListEntry, CharUnique, GameLogin, MAX_CONSTRUCTION_NUM, MAX_VILLAGE_NUMBER, MakeCharInfo,
    ServerInfo, VillageStruct, client_map_index, decode_make_new_char, decode_select_char,
    decode_village_struct_request, encode_del_result,
};
use gz_proto::item::{
    Item, MAX_ITEM_PER_PERSON, MAX_ITEM_UNIQUE, decode_inv_drop, decode_inv_pickup,
    decode_move_item, decode_split_item, encode_item_array, encode_person_item_info,
};
use gz_proto::opcodes_game::GameMsg;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

type AnyError = Box<dyn std::error::Error + Send + Sync>;

mod generated_capture;
mod generated_frames;
mod import_capture;

const DEFAULT_DATABASE_URL: &str = "postgres://postgres:Moon@2020@localhost:5432/goonzu";
const DEFAULT_LISTEN: &str = "0.0.0.0:4010";

/// `MAX_TOTAL_CHAR_PER_USER` (3 main + 12 char-house) — the legacy sends one
/// char-list packet for every slot, occupied or not
/// (`ServerDB-Person.cpp:1560-1606`). **All 15 are required:** the shipped
/// client's `IsReadyCharList` (`0x737120`) AND-reduces `clBI.GetIndex() == i`
/// across `i in 0..0xF` and only self-heals the missing slots after a long
/// timeout — sending fewer than 15 is what makes char-select stall. The array
/// is 15 entries (`PutCharList` `0x48D040` rejects `GetIndex() > 14`).
const CHAR_SLOTS: i16 = 15;

/// `CLIENT_COMPAT_VERSION` (`char.cpp:5180`). The client quits if this is
/// higher than its own build, so it must not track the real build number.
const CLIENT_COMPAT_VERSION: i16 = 1;

/// `ConstServiceArea_English` (`CommonHeader.h:102`). Must match the client's
/// `ServiceArea.txt` (`SERVICEAREA_ENGLISH`) or `DoMsg_..._SERVERINFO` drops the
/// packet and world entry stalls.
const SERVICE_AREA_ENGLISH: i16 = 0x08;

/// `siServerUnique` — arbitrary but stable for this single-world dev server.
const SERVER_UNIQUE: i16 = 1;

/// Base `uiTotalMoveSpeed` (6 bits).
const BASE_MOVE_SPEED: u8 = 8;

/// `< 0x4E20` (20000): the client indexes its `CharUniqueInfo` table by
/// `uiMakeCharUnique` (`SetCharReservedChar`).
const MAX_SESSION_CHAR_UNIQUE: u64 = 19_999;

#[tokio::main]
async fn main() -> Result<(), AnyError> {
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string());
    let listen = std::env::var("GAME_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.to_string());

    let mut argv = std::env::args().skip(1);
    match argv.next() {
        Some(command) => run_command(&command, &database_url, argv.collect()).await,
        None => serve(&database_url, &listen).await,
    }
}

/// Accepts client connections until the process is stopped.
async fn serve(database_url: &str, listen: &str) -> Result<(), AnyError> {
    let accounts = PgAccountRepository::connect(database_url).await?;
    let characters = PgCharacterRepository::connect(database_url).await?;
    let items = PgItemRepository::connect(database_url).await?;
    let listener = TcpListener::bind(listen).await?;
    println!("game: listening on {listen}");

    loop {
        let (stream, peer) = listener.accept().await?;
        let accounts = accounts.clone();
        let characters = characters.clone();
        let items = items.clone();
        let peer = peer.to_string();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, &peer, accounts, characters, items).await {
                eprintln!("game: {peer}: {e}");
            }
        });
    }
}

/// The non-serving modes, so seeding a database does not need a second binary.
///
/// `import-capture [--character <name>]` replaces the named character's
/// `character_items` rows with the item array from the replayed reference
/// capture (see [`import_capture`]).
async fn run_command(command: &str, database_url: &str, args: Vec<String>) -> Result<(), AnyError> {
    match command {
        "import-capture" => {
            let name = args
                .windows(2)
                .find(|w| w[0] == "--character")
                .map_or("Tester", |w| w[1].as_str());
            let characters = PgCharacterRepository::connect(database_url).await?;
            let items = PgItemRepository::connect(database_url).await?;
            let character = characters
                .find_by_name(name)
                .await?
                .ok_or_else(|| format!("no live character named {name:?}"))?;
            let report = import_capture::run(&items, character.character_id).await?;
            println!(
                "game: imported {} item(s) into {:?} (character_id {}); \
                 {} empty slot(s), {} with only a unique and a count",
                report.occupied,
                character.name,
                character.character_id,
                report.empty,
                report.sparse
            );
            Ok(())
        }
        other => Err(format!("unknown command {other:?} (try `import-capture`)").into()),
    }
}

async fn handle_client(
    mut stream: TcpStream,
    peer: &str,
    accounts: PgAccountRepository,
    characters: PgCharacterRepository,
    items: PgItemRepository,
) -> Result<(), AnyError> {
    let (key1, key2) = random_key_bytes();

    // S8: the client cannot send anything until it has this, and it decodes it
    // with the fixed link cipher (which it already has).
    let mut outbound = Cipher::link_cipher();
    // The character is not known yet — the login request that names it has not
    // arrived — so this value cannot be read from the database here. It is
    // pinned to the capture's character id because every per-character frame in
    // `generated_capture` identifies its subject by this number, and the client
    // silently discards records that do not match. `characters.game_unique`
    // already holds the durable per-character id and `handle_select_char`
    // cross-checks the two; the two stop being forced apart once
    // `RESPONSE_ALLCHARINFOTWO` is generated instead of replayed.
    let char_unique = generated_capture::CAPTURED_CHAR_UNIQUE;
    send_encoded(
        &mut stream,
        &mut outbound,
        GameMsg::ResponseClientcharunique,
        &CharUnique {
            char_unique,
            key1,
            key2,
            version: CLIENT_COMPAT_VERSION,
        }
        .encode(),
    )
    .await?;
    println!("game: {peer}: charunique={char_unique} -> ({key1:#04x}, {key2:#04x})");

    // Inbound frames use the session cipher, keyed by the bytes just sent.
    let mut session = Cipher::session_cipher(key1, key2);
    let mut parser = FrameParser::new();
    let mut buf = [0u8; 8192];
    // Set once the game login succeeds; select-char and later opcodes need it.
    let mut login: Option<GameSession> = None;
    // Cursor into `generated_capture::REPLY_PLAN`, so a repeated client opcode
    // can map to a different captured reply on its second appearance.
    let mut plan_step = 0usize;
    // Which captured frames get rebuilt from PostgreSQL on the way out.
    let mut live = LiveFrames {
        items: &items,
        character_id: None,
    };

    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            println!("game: {peer}: disconnected");
            return Ok(());
        }
        parser.feed(&buf[..n])?;

        while let Some(frame) = parser.next_frame()? {
            let wire = frame.as_bytes();
            let mut plain = vec![0u8; wire.len()];
            let integrity = session.decode(wire, &mut plain)?;
            if !integrity {
                eprintln!("game: {peer}: cipher desync at opcode {:#06x}", frame.cmd());
                return Ok(());
            }

            let (cmd, payload) = deframe(&plain)?;
            println!("game: {peer}: recv opcode {cmd} ({} bytes)", payload.len());

            match cmd {
                c if c == GameMsg::RequestLogin as u16 => {
                    let request = GameLogin::decode(payload.as_ref())?;
                    login = Some(
                        handle_game_login(
                            &mut stream,
                            &mut outbound,
                            &accounts,
                            &characters,
                            &request,
                        )
                        .await?,
                    );
                }
                c if c == GameMsg::RequestSelectchar as u16 => {
                    let slot = decode_select_char(payload.as_ref())?;
                    let character_id = {
                        let session = login
                            .as_ref()
                            .ok_or("selectchar before a successful game login")?;
                        handle_select_char(
                            &mut stream,
                            &mut outbound,
                            &characters,
                            session,
                            char_unique,
                            slot,
                        )
                        .await?
                    };
                    if let Some(session) = login.as_mut() {
                        session.character_id = Some(character_id.character_id);
                        session.entry_cell = Some(DropLocation {
                            map_id: character_id.map_id,
                            x: character_id.pos_x,
                            y: character_id.pos_y,
                        });
                    }
                    live.character_id = Some(character_id.character_id);
                    replay(
                        &mut stream,
                        &mut outbound,
                        generated_capture::PROLOGUE_TAIL,
                        "world-entry prologue",
                        &live,
                        // The character's first delivery of these frames, and the
                        // prologue carries no ALLCHARINFOONE anyway.
                        false,
                    )
                    .await?;
                }
                c if c == GameMsg::RequestItemsplit as u16 => {
                    let character_id = login
                        .as_ref()
                        .and_then(|s| s.character_id)
                        .ok_or("item split before a character was selected")?;
                    handle_item_split(
                        &mut stream,
                        &mut outbound,
                        &items,
                        character_id,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestMakenewchar as u16 => {
                    let (account_id, account_name) = login
                        .as_ref()
                        .map(|s| (s.account_id, s.username.clone()))
                        .ok_or("char create before a successful game login")?;
                    handle_make_new_char(
                        &mut stream,
                        &mut outbound,
                        &characters,
                        account_id,
                        &account_name,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestDeluserchar as u16 => {
                    let (account_id, account_name) = login
                        .as_ref()
                        .map(|s| (s.account_id, s.username.clone()))
                        .ok_or("char delete before a successful game login")?;
                    handle_userchar_deletion(
                        &mut stream,
                        &mut outbound,
                        &characters,
                        account_id,
                        &account_name,
                        GameMsg::ResponseDeluserchar,
                        true,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestPredeluserchar as u16 => {
                    let (account_id, account_name) = login
                        .as_ref()
                        .map(|s| (s.account_id, s.username.clone()))
                        .ok_or("char predelete before a successful game login")?;
                    handle_userchar_deletion(
                        &mut stream,
                        &mut outbound,
                        &characters,
                        account_id,
                        &account_name,
                        GameMsg::ResponsePredeluserchar,
                        false,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestVillagestruct as u16 => {
                    let village_ref = decode_village_struct_request(payload.as_ref())?;
                    handle_village_struct(&mut stream, &mut outbound, village_ref).await?;
                }
                c if c == GameMsg::RequestItemchange as u16 => {
                    // The client sends 305 exactly when a drag ends on an
                    // *occupied* slot — it is the merge/swap gesture. Legacy
                    // parses the same cltMoveItemInfo and runs the same
                    // sDBRequest_MoveItem path as 303, so one handler serves both.
                    let character_id = login
                        .as_ref()
                        .and_then(|s| s.character_id)
                        .ok_or("item change before a character was selected")?;
                    handle_item_move(
                        &mut stream,
                        &mut outbound,
                        &items,
                        character_id,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestItemmove as u16 => {
                    let character_id = login
                        .as_ref()
                        .and_then(|s| s.character_id)
                        .ok_or("item move before a character was selected")?;
                    handle_item_move(
                        &mut stream,
                        &mut outbound,
                        &items,
                        character_id,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestInvdrop as u16 => {
                    let (character_id, cell) = {
                        let session = login
                            .as_ref()
                            .ok_or("item drop before a successful game login")?;
                        let character_id = session
                            .character_id
                            .ok_or("item drop before a character was selected")?;
                        let cell = session
                            .entry_cell
                            .ok_or("item drop before a character position was known")?;
                        (character_id, cell)
                    };
                    handle_inv_drop(
                        &mut stream,
                        &mut outbound,
                        &items,
                        character_id,
                        cell,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestInvpickup as u16 => {
                    let character_id = login
                        .as_ref()
                        .ok_or("item pickup before a successful game login")?
                        .character_id
                        .ok_or("item pickup before a character was selected")?;
                    handle_inv_pickup(
                        &mut stream,
                        &mut outbound,
                        &items,
                        character_id,
                        payload.as_ref(),
                    )
                    .await?;
                }
                c if c == GameMsg::RequestChangestatusinfo as u16 => {
                    // The reference server answers 230 with a 25-frame data
                    // block (map/object bulk, then ALLCHARINFO one/two), not
                    // with 231. `REPLY_PLAN` mirrors that verbatim.
                    replay_plan_step(&mut stream, &mut outbound, cmd, &mut plan_step, &live)
                        .await?;
                }
                // The reference server sends nothing for 246; the data the client
                // wants arrives after 230.
                c if c == GameMsg::RequestAllcharinfo as u16 => {
                    println!("game: {peer}: allcharinfo request -> no reply (matches capture)");
                }
                other => println!("game: {peer}: unhandled opcode {other}"),
            }
        }
    }
}

/// Splits a decrypted frame into `(opcode, payload)`, decompressing when asked.
///
/// The client LZF-compresses every game-link frame of 200 bytes or more and
/// marks it with bit 15 of `usCmd` (`Client-Winsock.cpp:178-196`). The legacy
/// consumer decompresses and clears the flag before dispatch
/// (`GameProtocol.cpp:109-141`); without this the opcode reads as
/// `real | 0x8000` (e.g. `MAKENEWCHAR` 7 arrives as 32775). The front link
/// never compresses (`SendFrontServerMsg` has no LZF step), so only the game
/// service needs this.
///
/// # Errors
///
/// [`gz_lzf::LzfError`] if a flagged body is not a valid LZF stream, which the
/// caller treats as a fatal desync like a cipher failure.
fn deframe(plain: &[u8]) -> Result<(u16, Cow<'_, [u8]>), AnyError> {
    let cmd = u16::from_le_bytes([plain[3], plain[4]]);
    if cmd & COMPRESSION_FLAG == 0 {
        return Ok((cmd, Cow::Borrowed(&plain[HEADER_SIZE..])));
    }

    // A decompressed body can be no larger than the biggest frame a peer may
    // send, so size the scratch from the framing ceiling rather than guessing.
    let mut body = vec![0u8; MAX_FRAME_SIZE - HEADER_SIZE];
    let n = gz_lzf::decompress(&plain[HEADER_SIZE..], &mut body)?;
    body.truncate(n);
    Ok((cmd & !COMPRESSION_FLAG, Cow::Owned(body)))
}

/// The authenticated state carried for the rest of the connection.
struct GameSession {
    /// `accounts.account_id` (= the front's `siAuthKey`).
    account_id: i32,
    /// `accounts.gm_level`, forwarded as `siGameMasterLevel`.
    gm_level: i32,
    /// The world-entered character. `None` until `REQUEST_SELECTCHAR` names one,
    /// because the client picks a slot after logging in.
    character_id: Option<i32>,
    /// The logged-in account's name (`accounts.username`). Char-list/create
    /// messages echo this back to the client inside `cltCharListInfo.clBI.szAccountID`.
    username: String,
    /// Where the character stood on entering the world, used as the cell for
    /// `GAMEMSG_REQUEST_INVDROP`.
    ///
    /// This is deliberately *not* named `position`: the server does not track
    /// movement, so it stays at the entry coordinates no matter where the player
    /// walks. A dropped item therefore lands where the character spawned.
    entry_cell: Option<DropLocation>,
}

/// Sends every slot's `cltCharListInfo` (opcode 3), zeroed for empty slots, so
/// the client's `PutCharList` writes 15 copies and the roster dialog repaints.
async fn send_char_list(
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

/// S9–S11: verify the game login and answer with one char-list packet per slot.
async fn handle_game_login(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    accounts: &PgAccountRepository,
    characters: &PgCharacterRepository,
    login: &GameLogin,
) -> Result<GameSession, AnyError> {
    // The auth key is the account idnum the front handed out at login; a
    // mismatch means a stale or replayed key, so refuse rather than guess.
    let account = match accounts.find_by_username(&login.userid).await {
        Ok(account) => account,
        Err(RepositoryError::NotFound) => {
            return Err(format!("game login for unknown account {:?}", login.userid).into());
        }
        Err(e) => return Err(e.into()),
    };
    if account.account_id != login.auth_key {
        return Err(format!(
            "game login auth key {} does not match account {}",
            login.auth_key, account.account_id
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

    println!(
        "game: login ok for {:?} (account {}, auth_key {}): {occupied} character(s)",
        login.userid, account.account_id, login.auth_key
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
async fn handle_select_char(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
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
    // has never had one. It is *not* what goes on the wire yet: see
    // `session_char_unique`.
    let stored_unique = characters
        .resolve_game_unique(character.character_id)
        .await?;
    if stored_unique != char_unique {
        println!(
            "game: selectchar {:?} has game_unique {stored_unique} but this session opened with \
             {char_unique}; the replayed per-character frames will be discarded by the client",
            character.name
        );
    }

    // SERVERINFO sets `bReceiveServerInfoSwitch`, which `SetCharReservedChar`
    // checks before it will create any avatar. It must precede RESPONSE_LOGIN.
    send_encoded(
        stream,
        outbound,
        GameMsg::ResponseServerinfo,
        &default_server_info().encode(),
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
        total_move_speed: BASE_MOVE_SPEED,
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

    println!(
        "game: selectchar slot {slot} char {:?} -> map {} at ({}, {})",
        character.name, character.map_id, character.pos_x, character.pos_y
    );
    Ok(character)
}

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
async fn handle_item_move(
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
    println!("game: itemmove {from} -> {to} for character {character_id}: {detail}",);

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
async fn handle_item_split(
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
    println!("game: itemsplit {from} -> {to} x{count} for character {character_id}: {detail}");

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

/// Applies `GAMEMSG_REQUEST_MAKENEWCHAR` (7): persist a new character and
/// echo its `cltCharListInfo` with a success response (opcode 8).
async fn handle_make_new_char(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    characters: &PgCharacterRepository,
    account_id: i32,
    account_name: &str,
    payload: &[u8],
) -> Result<(), AnyError> {
    let (name, sex, class_kind, slot) = decode_make_new_char(payload)?;
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
    println!("game: created char {name:?} id={character_id} slot={slot} for account {account_id}");
    Ok(())
}

/// Applies `GAMEMSG_REQUEST_DELUSERCHAR` (9) and
/// `GAMEMSG_REQUEST_PREDELUSERCHAR` (83): the final "yes, delete" and the
/// grace-mark request. Both reach the same slot-clear response (opcodes 10/84).
#[allow(clippy::too_many_arguments)]
async fn handle_userchar_deletion(
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
        println!("game: char-del slot {slot} out of range for account {account_id}");
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
            println!("game: char-del slot {slot} id {id} for account {account_id}: done");
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
            println!("game: char-del slot {slot} for account {account_id}: slot already empty");
        }
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
async fn handle_inv_drop(
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
        println!(
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
            format!("drop cell ({x}, {y}) and all eight neighbours are occupied")
        }
    };
    println!("game: invdrop slot {slot} for character {character_id}: {detail}");

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
        println!(
            "game: sent itemdrop 309: map {} ({}, {}) unique {} handle {}",
            grounded.map_index, grounded.x, grounded.y, grounded.item_unique, grounded.list_index
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
async fn handle_inv_pickup(
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
        println!(
            "game: ignoring invpickup of unique {} at handle {} (outside the legacy's range)",
            request.item_unique, request.item_index
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
                println!("game: invpickup reported success without a slot or item");
                return Ok(());
            }
        },
        PickupStatus::NoSuchItem { handle } => {
            println!("game: invpickup: no ground item has handle {handle}");
            return Ok(());
        }
        PickupStatus::ItemMismatch { stored, claimed } => {
            println!("game: invpickup: handle names unique {stored}, not {claimed}");
            return Ok(());
        }
        PickupStatus::CellMismatch { stored, claimed } => {
            println!(
                "game: invpickup: item is at ({}, {}, {}), client said ({}, {}, {})",
                stored.0, stored.1, stored.2, claimed.0, claimed.1, claimed.2
            );
            return Ok(());
        }
        PickupStatus::NoFreeSlot => {
            println!("game: invpickup: every carry slot is occupied");
            return Ok(());
        }
    };

    println!(
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
        println!(
            "game: sent itemdrop 309 removal for map {} ({}, {})",
            cleared.map_index, cleared.x, cleared.y
        );
    }

    Ok(())
}

/// Answers `GAMEMSG_REQUEST_VILLAGESTRUCT` (1101). The client blocks player
/// movement (`OrderAction`) and stops popping reserved characters
/// (`Client-SetChar.cpp:66`) until this arrives, so a village map that never
/// gets an answer looks frozen. Zero structure info renders the empty lots.
async fn handle_village_struct(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    village_ref: i32,
) -> Result<(), AnyError> {
    if village_ref <= 0 || village_ref >= i32::from(MAX_VILLAGE_NUMBER) {
        println!("game: ignoring village-struct request for ref {village_ref}");
        return Ok(());
    }

    let body = VillageStruct {
        village_unique: i16::try_from(village_ref).unwrap_or(0),
        structure_info: [0; MAX_CONSTRUCTION_NUM],
    };
    send_encoded(
        stream,
        outbound,
        GameMsg::ResponseVillagestruct,
        &body.encode(),
    )
    .await?;
    println!("game: sent village {village_ref} structure");
    Ok(())
}

/// The `GAMEMSG_RESPONSE_SERVERINFO` body for this single-world dev server.
fn default_server_info() -> ServerInfo {
    ServerInfo {
        service_area: SERVICE_AREA_ENGLISH,
        server_unique: SERVER_UNIQUE,
        item_mall_use: false,
        village_war: false,
        bonus_time: false,
        bonus2_time: false,
        special_quest_bonus_time: false,
        broadcast: false,
        best_village_level: 0,
        npc_price_rate: 100,
        item_price_index: 0,
        event_switch: [false; 8],
        minimum_teacher_level: 0,
        service_age: 0,
        enable_pvp: 0,
        auto_unblock_mode: false,
        vote_campaign: false,
        event_toggle_switch: [false; 2],
    }
}

async fn send_encoded(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    cmd: GameMsg,
    payload: &[u8],
) -> Result<(), AnyError> {
    send_raw(stream, cipher, cmd as u16, payload).await
}

/// Writes one already-framed body under a numeric opcode.
///
/// Needed for the capture replay: many captured frames have no `GameMsg`
/// variant, and the opcode on the wire is what the client dispatches on anyway.
async fn send_raw(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    cmd: u16,
    payload: &[u8],
) -> Result<(), AnyError> {
    let frame = Frame::new(cmd, payload)?;
    let wire = frame.into_bytes();
    let enciphered = cipher.encode(&wire)?;
    stream.write_all(&enciphered).await?;
    stream.flush().await?;
    Ok(())
}

/// Whether a captured frame has to be withheld when the client re-asks for state
/// it already has.
///
/// Two frames in the 230 burst qualify, for two unrelated reasons.
///
/// ## 248 `RESPONSE_ALLCHARINFOONE` — it teleports the player
///
/// 248 is the only frame the client acts on by writing `cltPBasicInfo`, and
/// `cltPBasicInfo` embeds `cltCharPos clPos`. `SetOne` copies the whole struct
/// (`Client-Person.cpp:776` -> `PersonInfo.h:589`), and the character's own
/// coordinates are read back out of exactly that field: `cltCharCommon::GetX()`
/// is `return pclCI->clBI.GetX();` (`Char-Common.cpp:1206`), which is
/// `clPos.GetX()` (`Common.cpp:2917`), i.e. `clPos.siX`. So a verbatim 248 moves
/// the client's idea of where it is standing back to the cell the capture was
/// taken on. The client sends 230 after every mutation, so replaying 248 there is
/// what snapped the player back to spawn on a drop.
///
/// Nothing else in the burst does this. 249 goes through `SetTwo`, which touches
/// only `clCharItem` and `clSummonInfo` (`PersonInfo.h:617`), and `SetOne` is the
/// sole `clBI` writer anywhere in the client. Withholding 248 also costs the
/// post-mutation refresh nothing: the inventory weight bar reads
/// `cltCharCommon::GetTotalWeight()` -> `pclCI->clCharItem.GetTotalWeight(...)`
/// (`Char-Common.cpp:2791`), summing the item array that 249 carries, not a
/// cached total held in `clBI`.
///
/// The step's first delivery is untouched, and there the captured position is
/// the truth: the character is still on the entry cell that `RESPONSE_LOGIN`
/// placed it on. Tracking movement -- decoding `REQUEST_USERORDER`, whose
/// `ORDER_MOVE` payload carries the destination as `x | (y << 16)` in `siPara1`
/// (`order.h:96`) -- is what puts 248 back for re-syncs.
///
/// ## 116 `RESPONSE_LETTER` — it re-shows the memo dialog
///
/// 116 is a one-shot notification, not state. Its handler
/// (`Client-System.cpp:2490`) has no "already seen" test: it plays
/// `SOUND_EFFECT_LETTER`, creates `NLETTER_DLG`, calls `Push(buffer)` and flashes
/// the taskbar. Replaying it therefore re-announces the same captured letter --
/// the bank dividend memo -- every time the client re-asks for status, which it
/// does on a timer as well as after every mutation. That is the source of the
/// once-a-minute dialog; it is not a clock problem, and no amount of adjusting
/// time frames touches it.
///
/// Withholding it costs nothing, because the letter the client is being reminded
/// about is the one it already has: 116 is the only letter frame in the capture,
/// so there is no later frame that would have delivered it instead.
///
/// Note this is unrelated to `RESPONSE_DATE` (1208) and
/// `RESPONSE_SERVERTIME` (177), the two time frames the same burst carries. Both
/// replay a *frozen* timestamp rather than a live one, so they are stale rather
/// than spamming; see the clock notes on [`replay`].
fn withhold_on_resync(cmd: u16) -> bool {
    cmd == GameMsg::ResponseAllcharinfoone as u16 || cmd == GameMsg::ResponseLetter as u16
}

/// Replays the captured frames in the half-open index range `start..end`.
///
/// `resync` marks a replay of a step the client already received, which is the
/// one case where a captured frame is not merely stale but actively wrong; see
/// [`withhold_on_resync`].
///
/// # Errors
///
/// Propagates any cipher or socket failure; a write failure ends the session.
async fn replay(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    (start, end): (u32, u32),
    what: &str,
    live: &LiveFrames<'_>,
    resync: bool,
) -> Result<(), AnyError> {
    let frames: Vec<(u16, &'static [u8])> = generated_capture::frames_in(start, end).collect();
    let total: usize = frames.iter().map(|(_, body)| body.len()).sum();
    println!(
        "game: replaying {what}: {} frames, {total} payload bytes",
        frames.len()
    );
    for (cmd, body) in frames {
        if resync && withhold_on_resync(cmd) {
            // The reason differs per frame -- see `withhold_on_resync` -- and
            // logging one reason for both sent people hunting for the wrong bug.
            let why = if cmd == GameMsg::ResponseAllcharinfoone as u16 {
                "it would reset our position"
            } else {
                "it would re-announce a notification we already sent"
            };
            println!("game: withholding {what} frame {cmd}: {why}");
            continue;
        }
        match live.regenerate(cmd, body).await? {
            Some(generated) => send_raw(stream, cipher, cmd, &generated).await?,
            None => send_raw(stream, cipher, cmd, body).await?,
        }
    }
    Ok(())
}

/// Advances the capture replay plan when `cmd` is the step the reference server
/// answered at this point, and does nothing otherwise.
///
/// Walking the plan as a cursor rather than a lookup keeps the captured
/// one-to-one ordering: opcode 121 gets a different reply the second time the
/// client sends it.
///
/// A step the client already consumed can still be replayed if it asks for it
/// again; see the repeat branch below for why `230` depends on that.
///
/// # Errors
///
/// Propagates any cipher or socket failure from [`replay`].
async fn replay_plan_step(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    cmd: u16,
    plan_step: &mut usize,
    live: &LiveFrames<'_>,
) -> Result<(), AnyError> {
    let Some((expect, ranges)) = generated_capture::REPLY_PLAN.get(*plan_step) else {
        return Ok(());
    };
    if *expect == cmd {
        *plan_step += 1;
        let label = format!("plan step {expect}");
        for (start, end) in *ranges {
            replay(stream, cipher, (*start, *end), &label, live, false).await?;
        }
        return Ok(());
    }

    // Not the next step. If the client is asking again for a step it already got,
    // that is a re-sync rather than a protocol error: `REQUEST_CHANGESTATUSINFO`
    // (230) means "send me current state", and the client sends it every time it
    // wants a fresh answer — after a move, after a teleport. Answering it again is
    // what makes a mutation observable at all, since `LiveFrames` re-reads
    // PostgreSQL on the way out.
    //
    // The nearest earlier step wins, so a repeat of 230 cannot consume the 121
    // entry that follows it and shift every later pairing.
    //
    // `resync` is set because a repeat is the one delivery where the captured
    // bytes are not just out of date but would move the player: 248 in the 230
    // burst carries the capture's `clPos`, and the client reads its own
    // coordinates straight out of that. See [`withhold_on_resync`].
    if let Some(step) = (0..*plan_step)
        .rev()
        .find(|&i| generated_capture::REPLY_PLAN[i].0 == cmd)
    {
        let label = format!("plan step {cmd} (repeat)");
        for (start, end) in generated_capture::REPLY_PLAN[step].1 {
            replay(stream, cipher, (*start, *end), &label, live, true).await?;
        }
        return Ok(());
    }

    println!("game: replay plan waiting for opcode {expect}, client sent {cmd}");
    Ok(())
}

/// The captured frames this server owns rather than replays.
///
/// Every frame in the burst still comes from `login_capture.pcapng`; the point
/// is only that the parts we have modelled are read back out of PostgreSQL on
/// the way out, so a change the player makes in the world survives the next
/// login. Un-modelled frames stay byte-exact, which is what keeps the client
/// accepting the burst.
struct LiveFrames<'a> {
    items: &'a PgItemRepository,
    /// `None` until `REQUEST_SELECTCHAR` has named the character, which is when
    /// the burst carrying the item array is first sent.
    character_id: Option<i32>,
}

impl LiveFrames<'_> {
    /// Rebuilds `body` for the frames we own, or returns `None` to replay it
    /// verbatim.
    ///
    /// # Errors
    ///
    /// [`gz_db::RepositoryError`] from the item read, or a stored record that is
    /// not a whole `cltItem`.
    async fn regenerate(&self, cmd: u16, body: &'static [u8]) -> Result<Option<Vec<u8>>, AnyError> {
        if cmd != GameMsg::ResponseAllcharinfotwo as u16 {
            return Ok(None);
        }
        let Some(character_id) = self.character_id else {
            return Ok(None);
        };

        let stored = self.items.list(character_id).await?;
        let mut array = vec![Item::empty(); MAX_ITEM_PER_PERSON];
        for row in stored {
            let Some(cell) = array.get_mut(usize::from(row.slot)) else {
                return Err(format!(
                    "stored item slot {} is outside 0..{MAX_ITEM_PER_PERSON}",
                    row.slot
                )
                .into());
            };
            *cell = Item::decode(&row.raw)?;
        }

        let mut regenerated = body.to_vec();
        encode_item_array(&mut regenerated, &array)?;
        Ok(Some(regenerated))
    }
}

/// Two key bytes for this connection's session cipher. Unpredictability only
/// needs to defeat accidental reuse in development, not an attacker.
fn random_key_bytes() -> (u8, u8) {
    let v = hash_now();
    (v as u8, (v >> 8) as u8)
}

/// A positive session character id below the client's `CharUniqueInfo` bound
/// (`< 0x4E20`). The value is also written into
/// `clMakeCharInfo.uiMakeCharUnique` (a 16-bit field) and compared against the
/// client's 32-bit `MyCharUnique`, so it must fit both.
const _: () = assert!(
    generated_capture::CAPTURED_CHAR_UNIQUE > 0
        && (generated_capture::CAPTURED_CHAR_UNIQUE as u64) < MAX_SESSION_CHAR_UNIQUE
);

fn hash_now() -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .hash(&mut hasher);
    std::thread::current().id().hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deframe_leaves_an_uncompressed_body_alone() {
        let plain = Frame::new(GameMsg::RequestSelectchar as u16, &7i32.to_le_bytes())
            .unwrap()
            .into_bytes();
        let (cmd, payload) = deframe(&plain).unwrap();
        assert_eq!(cmd, GameMsg::RequestSelectchar as u16);
        assert_eq!(payload.as_ref(), 7i32.to_le_bytes().as_slice());
    }

    #[test]
    fn deframe_decompresses_and_clears_the_flag() {
        // The client compresses game frames >= 200 bytes and sets bit 15
        // (`Client-Winsock.cpp:178-196`); the real client's MAKENEWCHAR arrived
        // as opcode 32775, i.e. 7 | 0x8000.
        let body = vec![0x41u8; 400];
        let mut compressed = vec![0u8; body.len() * 2];
        let n = gz_lzf::compress(&body, &mut compressed).unwrap();
        compressed.truncate(n);

        assert!(compressed.len() < 200, "test body should compress");

        let total = HEADER_SIZE + compressed.len();
        let mut plain = vec![0u8; total];
        plain[..2].copy_from_slice(&(total as u16).to_le_bytes());
        plain[2] = 0;
        plain[3..5].copy_from_slice(
            &(GameMsg::RequestMakenewchar as u16 | COMPRESSION_FLAG).to_le_bytes(),
        );
        plain[HEADER_SIZE..].copy_from_slice(&compressed);

        let (cmd, payload) = deframe(&plain).unwrap();
        assert_eq!(cmd, GameMsg::RequestMakenewchar as u16);
        assert_eq!(payload.as_ref(), body.as_slice());
    }

    /// The splice into `RESPONSE_ALLCHARINFOTWO` has to be byte-transparent.
    ///
    /// The client's willingness to accept the world-entry burst rests on that
    /// frame being exactly what the reference server sent; if the DB holds what
    /// the importer read out of the capture, regenerating it must reproduce the
    /// captured bytes and change nothing else.
    #[test]
    fn regenerating_the_capture_reproduces_it_byte_for_byte() {
        use gz_proto::item::{ITEM_ARRAY_LEN, decode_item_array};

        let body = generated_capture::frame(GameMsg::ResponseAllcharinfotwo as u16)
            .expect("the capture must contain an ALLCHARINFOTWO frame");

        // What `LiveFrames::regenerate` does once the stored rows come back.
        let decoded = decode_item_array(body).unwrap();
        let mut rebuilt = body.to_vec();
        encode_item_array(&mut rebuilt, &decoded).unwrap();

        assert_eq!(rebuilt, body);
        assert_eq!(body.len(), ITEM_ARRAY_LEN + 4240);
    }

    /// A re-sync has to keep the inventory refresh and lose the position reset.
    ///
    /// Withholding 248 is only safe because 249 -- the frame that carries the item
    /// array `LiveFrames` re-reads from PostgreSQL -- is a different frame. If the
    /// two ever collapsed into one, a drop would stop showing up in the bag.
    #[test]
    fn a_resync_drops_only_the_position_carrying_frame() {
        let step = generated_capture::REPLY_PLAN
            .iter()
            .find(|(cmd, _)| *cmd == GameMsg::RequestChangestatusinfo as u16)
            .map(|(_, ranges)| *ranges)
            .expect("the plan must answer a state sync");

        let sent: Vec<u16> = step
            .iter()
            .flat_map(|&(start, end)| generated_capture::frames_in(start, end))
            .map(|(cmd, _)| cmd)
            .collect();

        // The reference burst carries each of these exactly once.
        for cmd in [
            GameMsg::ResponseAllcharinfoone as u16,
            GameMsg::ResponseAllcharinfotwo as u16,
        ] {
            assert_eq!(
                sent.iter().filter(|&&c| c == cmd).count(),
                1,
                "expected exactly one {cmd} in the state-sync burst"
            );
        }

        let withheld: Vec<u16> = sent
            .iter()
            .copied()
            .filter(|&cmd| withhold_on_resync(cmd))
            .collect();
        assert_eq!(
            withheld,
            vec![
                GameMsg::ResponseAllcharinfoone as u16,
                GameMsg::ResponseLetter as u16,
            ]
        );

        // And what is left still refreshes the bag.
        let kept: Vec<u16> = sent
            .iter()
            .copied()
            .filter(|&cmd| !withhold_on_resync(cmd))
            .collect();
        assert!(kept.contains(&(GameMsg::ResponseAllcharinfotwo as u16)));
        assert_eq!(sent.len(), 25, "the captured state-sync burst is 25 frames");
        assert_eq!(kept.len(), sent.len() - 2);
    }

    /// The memo dialog repeats because 116 is replayed, not because of the clock.
    ///
    /// This is the regression test for the once-a-minute popup: it asserts the
    /// captured burst really does carry a letter, so that withholding 116 has
    /// something to withhold. Without this the fix could be silently reverted by a
    /// regeneration that keeps the frame.
    #[test]
    fn the_state_sync_burst_carries_the_letter_that_repopped_the_dialog() {
        let step = generated_capture::REPLY_PLAN
            .iter()
            .find(|(cmd, _)| *cmd == GameMsg::RequestChangestatusinfo as u16)
            .map(|(_, ranges)| *ranges)
            .expect("the plan must answer a state sync");

        let letters: Vec<u16> = step
            .iter()
            .flat_map(|&(start, end)| generated_capture::frames_in(start, end))
            .map(|(cmd, _)| cmd)
            .filter(|&cmd| cmd == GameMsg::ResponseLetter as u16)
            .collect();

        assert_eq!(
            letters.len(),
            1,
            "the capture carries exactly one letter; a second would mean \
             withholding it on re-sync could lose a genuine new memo"
        );
        assert!(withhold_on_resync(GameMsg::ResponseLetter as u16));
    }

    /// The two time frames in the burst are stale, not spammy — so they stay.
    ///
    /// They replay a timestamp frozen at capture time rather than a live one. That
    /// is a separate defect from the dialog spam, and fixing it means generating a
    /// live clock rather than replaying one, so neither is withheld here.
    #[test]
    fn the_time_frames_are_frozen_rather_than_withheld() {
        assert!(!withhold_on_resync(GameMsg::ResponseDate as u16));
        assert!(!withhold_on_resync(GameMsg::ResponseServertime as u16));

        // Confirm the "frozen" half of that claim rather than asserting it: both
        // frames must decode to the capture-time instant, and 1208 must be the
        // 4-byte packed cltDate rather than a wider struct.
        let date = generated_capture::frame(GameMsg::ResponseDate as u16)
            .expect("the capture must contain a RESPONSE_DATE frame");
        assert_eq!(date.len(), 4, "cltDate is a single packed bitfield word");

        let stime = generated_capture::frame(GameMsg::ResponseServertime as u16)
            .expect("the capture must contain a RESPONSE_SERVERTIME frame");
        assert_eq!(stime.len(), 16, "_SYSTEMTIME is eight 16-bit words");
    }

    /// The item array a resync still sends has to be the whole modelled range.
    ///
    /// `regenerating_the_capture_reproduces_it_byte_for_byte` proves the splice
    /// is byte-transparent; this pins that the frame reaching the client after a
    /// drop is the one the splice wrote, i.e. that withholding 248 did not also
    /// withhold the refresh.
    #[test]
    fn the_captured_state_sync_still_carries_a_full_item_array() {
        let body = generated_capture::frame(GameMsg::ResponseAllcharinfotwo as u16)
            .expect("the capture must contain an ALLCHARINFOTWO frame");
        let items = gz_proto::item::decode_item_array(body).unwrap();
        assert_eq!(items.len(), gz_proto::item::MAX_ITEM_PER_PERSON);
        // The capture was taken with the reference character's kit on, so the
        // frame is not trivially empty and would be visibly wrong if dropped.
        assert!(items.iter().any(|i| !i.is_empty()));
    }

    /// Pins the item layout facts the importer and the encoder both rest on.
    #[test]
    fn the_captured_item_array_is_eighty_four_byte_records() {
        use gz_proto::item::{Item, decode_item_array, personitem::*};

        let body = generated_capture::frame(GameMsg::ResponseAllcharinfotwo as u16).unwrap();
        let items = decode_item_array(body).unwrap();
        assert_eq!(items.len(), MAX_ITEM_PER_PERSON);

        // Slot 0 is the helmet the reference character had equipped. These are
        // the values `0003`'s importer wrote for character 1.
        assert_eq!(items[HELMET as usize].unique(), 705);
        assert_eq!(items[HELMET as usize].count(), 1);
        assert_eq!(items[WEAPON1 as usize].unique(), 1);

        // The reference character carries a nearly full bag: 19 of the 20 bag
        // slots, with only 20 empty. That occupancy is what pinned the bag at 20
        // wide -- read as the six slots `ItemUnit.h` declares, slots 21..=34 would
        // be an unexplained run of 14 more items in a range with no name.
        let bag: Vec<u16> = (INV0..=INV_END)
            .filter(|&slot| !items[slot as usize].is_empty())
            .collect();
        assert_eq!(bag.len(), 19);
        assert_eq!(bag.first(), Some(&INV0));
        assert_eq!(bag.last(), Some(&INV_END));
        assert!(
            items[20usize].is_empty(),
            "slot 20 is the only free bag slot"
        );
        assert!(bag.contains(&24) && bag.contains(&28));

        // The horse holds 13 of its 20 slots; both extra bags are empty, which is
        // consistent with a character carrying no bag-expansion items.
        let horse: Vec<u16> = (HORSE0..=HORSE_END)
            .filter(|&slot| !items[slot as usize].is_empty())
            .collect();
        assert_eq!(horse.len(), 13);
        assert!(
            items[BAG0 as usize..=BAG0_END as usize]
                .iter()
                .all(Item::is_empty)
        );
        assert!(
            items[BAG1 as usize..=BAG1_END as usize]
                .iter()
                .all(Item::is_empty)
        );

        // Slot 79 is outside every container: the array is 80 long but only 79
        // slots are addressable.
        assert!(items[BAG1_END as usize + 1..].iter().all(Item::is_empty));
    }
}
