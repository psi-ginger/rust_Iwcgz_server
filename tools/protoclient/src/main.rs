//! Scripted protocol client for the M1 login/entry path.
//!
//! Walks the same front-server exchange the real client does (blueprint 05
//! §S1–S6) so the service can be driven end to end without the GUI, and so
//! every step is a deterministic pass/fail instead of "the client looked
//! okay". It is the harness that must go green before `New Era.exe` is
//! pointed at the service.
//!
//! Usage: `protoclient [front_addr] [user] [password] [--move-item <from>:<to>] [--drop-item <slot>:<count>]`
//! Defaults: `127.0.0.1:5700 test test`.
//!
//! `--move-item` runs the inventory phase after world entry: it pulls the
//! `RESPONSE_ALLCHARINFOTWO` item array the server generated from PostgreSQL,
//! sends `GAMEMSG_REQUEST_ITEMMOVE`, and pulls the array again to check the move
//! was persisted and re-read. That is the whole item-mutation round trip, minus
//! the GUI's local optimistic update.
//!
//! `--use-bonus <pos>:<amount>` runs the stat-allocation round trip: it sends
//! `GAMEMSG_REQUEST_USEBONUS` (250) and asserts the `GAMEMSG_RESPONSE_PERSON_IP`
//! (215) the server replies with — the same frame the GUI status window applies —
//! carries the accepted stat, the decremented bonus and the recomputed maxima.
//! The assertions assume a fresh dev seed (`scripts\db-init.ps1`) run before the
//! probe and exactly one spend. Pass `--expect-refused` to instead assert the
//! refusal path: no 215 arrives and the session survives (proven by a
//! `REQUEST_CHANGESTATUSINFO` round trip after the refused spend).

use gz_cipher::Cipher;
use gz_net::{Frame, FrameParser, HEADER_SIZE};
use gz_proto::game::{MAKE_CHAR_INFO_PAYLOAD_LEN, MakeCharInfo};
use gz_proto::item::{CLT_ITEM_SIZE, Item};
use gz_proto::opcodes_front::FrontMsg;
use gz_proto::opcodes_game::GameMsg;
use gz_proto::order::{BasicCharUpdate, OrderData};
use gz_proto::wire::{Reader, Writer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

type AnyError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), AnyError> {
    let mut args = std::env::args().skip(1);
    let addr = args.next().unwrap_or_else(|| "127.0.0.1:5700".to_string());
    let user = args.next().unwrap_or_else(|| "test".to_string());
    let password = args.next().unwrap_or_else(|| "test".to_string());

    // Optional `--move-item <from>:<to>`, the two absolute `cltPItemInfo::clItem`
    // indices the client would put in `cltMoveItemInfo`.
    let rest = args.collect::<Vec<_>>();
    let move_item = rest
        .windows(2)
        .find(|w| w[0] == "--move-item")
        .map(|w| {
            let (from, to) = w[1]
                .split_once(':')
                .ok_or("expected --move-item <from>:<to>")?;
            Ok::<_, AnyError>((
                from.parse::<u16>()
                    .map_err(|_| "from is not a slot index")?,
                to.parse::<u16>().map_err(|_| "to is not a slot index")?,
            ))
        })
        .transpose()?;

    // Optional `--drop-item <slot>:<count>[:<claim_unique>]`, the
    // `cltGameMsgRequest_InvDrop` body: one absolute slot and how much of that
    // stack to remove. The optional third field puts a *wrong* `siUnique` in the
    // request, which is how a desynced client looks; the probe then expects the
    // server to refuse.
    let drop_item = rest
        .windows(2)
        .find(|w| w[0] == "--drop-item")
        .map(|w| {
            let mut parts = w[1].split(':');
            let slot = parts
                .next()
                .ok_or("expected --drop-item <slot>:<count>")?
                .parse::<u16>()
                .map_err(|_| "slot is not a slot index")?;
            let count = parts
                .next()
                .ok_or("expected --drop-item <slot>:<count>")?
                .parse::<u16>()
                .map_err(|_| "count is not a number")?;
            let claim = parts
                .next()
                .map(|s| s.parse::<u16>().map_err(|_| "claim_unique is not a number"))
                .transpose()?;
            Ok::<_, AnyError>((slot, count, claim))
        })
        .transpose()?;

    // Optional `--use-bonus <pos>:<amount>`, the `cltGameMsgResponse_UseBonus`
    // body: ability `siPos` (0..=6, see `NCharPerson.cpp:2139-2163`) and `siAmount`.
    // `--expect-refused` flips it from "assert the 215" to "assert the refusal":
    // no 215, and the session still answers a later request.
    let use_bonus = rest
        .windows(2)
        .find(|w| w[0] == "--use-bonus")
        .map(|w| {
            let (pos, amount) = w[1]
                .split_once(':')
                .ok_or("expected --use-bonus <pos>:<amount>")?;
            Ok::<_, AnyError>((
                pos.parse::<i32>()
                    .map_err(|_| "pos is not an ability index")?,
                amount
                    .parse::<i16>()
                    .map_err(|_| "amount is not a number")?,
                rest.iter().any(|a| a == "--expect-refused"),
            ))
        })
        .transpose()?;

    // Optional `--observe-char-update <observer_user>:<observer_password>`: hold a
    // second account in the world and assert the 200 fan reaches it (slice 6 B1).
    // The primary session moves; the observer must receive `RESPONSE_CHARUPDATE`
    // frames naming the *mover's* id and tracking the stepped cell.
    let observe_char_update = rest
        .windows(2)
        .find(|w| w[0] == "--observe-char-update")
        .map(|w| {
            let (user, password) = w[1]
                .split_once(':')
                .ok_or("expected --observe-char-update <user>:<password>")?;
            Ok::<_, AnyError>((user.to_owned(), password.to_owned()))
        })
        .transpose()?;

    println!("probe: connecting to {addr} as {user:?}");
    let mut stream = TcpStream::connect(&addr).await?;
    let mut parser = FrameParser::new();
    let mut buf = [0u8; 4096];

    // S1: secret key, sent unprompted and in the clear.
    let frame = read_frame(&mut stream, &mut parser, &mut buf).await?;
    expect_cmd(&frame, FrontMsg::ResponseSecretkey)?;
    let key1 = frame.payload()[2];
    let key2 = frame.payload()[4];
    println!("probe: got secret key ({key1:#04x}, {key2:#04x})");
    let mut cipher = Cipher::session_cipher(key1, key2);

    // S2: English login (opcode 21), ciphered.
    send_encoded(
        &mut stream,
        &mut cipher,
        FrontMsg::RequestEnglishLogin,
        &english_login(&user, &password),
    )
    .await?;
    let frame = read_frame(&mut stream, &mut parser, &mut buf).await?;
    expect_cmd(&frame, FrontMsg::ResponseLogin)?;
    let mut r = Reader::new(frame.payload());
    let result = r.u16()?;
    let auth_key = r.i32()?;
    let echoed_user = r.fixed_str(20)?;
    let _gm = r.i16()?;
    println!("probe: login result={result} auth_key={auth_key} user={echoed_user:?}");
    if result != 0 {
        return Err(format!("login failed with result {result}").into());
    }

    // S5: notice (no request payload), then S6: world list.
    send_encoded(&mut stream, &mut cipher, FrontMsg::RequestNotice, &[]).await?;
    let frame = read_frame(&mut stream, &mut parser, &mut buf).await?;
    expect_cmd(&frame, FrontMsg::ResponseNotice)?;
    println!("probe: notice ok ({} bytes)", frame.payload().len());

    let mut w = Writer::new();
    w.fixed_str(&user, 20);
    send_encoded(
        &mut stream,
        &mut cipher,
        FrontMsg::RequestWorldlist,
        &w.finish(),
    )
    .await?;
    let frame = read_frame(&mut stream, &mut parser, &mut buf).await?;
    expect_cmd(&frame, FrontMsg::ResponseWorldlist)?;
    let payload = frame.payload();
    let count = u16::from_le_bytes([payload[0], payload[1]]);
    println!("probe: world list has {count} world(s)");
    if count == 0 {
        return Err("world list was empty".into());
    }
    let entry = &payload[2..];
    let world_id = u16::from_le_bytes([entry[0], entry[1]]);
    let world_name = Reader::new(&entry[2..22]).fixed_str(20).unwrap_or_default();
    let ip = Reader::new(&entry[28..44])
        .fixed_str(16)
        .unwrap_or_default();
    let port = u16::from_le_bytes([entry[44], entry[45]]);
    println!("probe: world {world_id} {world_name:?} -> {ip}:{port}");

    // S7: reconnect to the game server the world list advertised.
    let game_addr = format!("{ip}:{port}");
    println!("probe: connecting to game server {game_addr}");
    let mut game = TcpStream::connect(&game_addr).await?;
    let mut gparser = FrameParser::new();
    let mut gbuf = [0u8; 8192];
    // Server->client is encoded with the fixed link cipher; client->server uses
    // the session cipher keyed by CLIENTCHARUNIQUE.
    let mut link_rx = Cipher::link_cipher();

    // S8: CLIENTCHARUNIQUE.
    let (cmd, plain) = read_game_frame(&mut game, &mut gparser, &mut gbuf, &mut link_rx).await?;
    expect_game_cmd(cmd, GameMsg::ResponseClientcharunique)?;
    let mut r = Reader::new(&plain[HEADER_SIZE..]);
    let char_unique = r.i32()?;
    let gkey1 = r.u8()?;
    let gkey2 = r.u8()?;
    let gversion = r.i16()?;
    println!("probe: charunique={char_unique} key=({gkey1:#04x}, {gkey2:#04x}) version={gversion}");
    if gversion > 1 {
        return Err(format!("server advertises version {gversion}; client is 1").into());
    }
    let mut session_tx = Cipher::session_cipher(gkey1, gkey2);

    // S9: game login.
    let mut w = Writer::with_capacity(48);
    w.i32(auth_key);
    w.fixed_str(&user, 20);
    w.fixed_str(&password, 20);
    w.u8(0);
    w.pad(3);
    send_game_encoded(
        &mut game,
        &mut session_tx,
        GameMsg::RequestLogin,
        &w.finish(),
    )
    .await?;

    // S10/S11: one char-list packet per slot (occupied or empty). The client
    // requires all `MAX_TOTAL_CHAR_PER_USER` = 15 (`IsReadyCharList`, 0x737120).
    let mut occupied = 0;
    for _ in 0..15 {
        let (cmd, plain) =
            read_game_frame(&mut game, &mut gparser, &mut gbuf, &mut link_rx).await?;
        expect_game_cmd(cmd, GameMsg::ResponseCharlist)?;
        let p = &plain[HEADER_SIZE..];
        let slot = p[24];
        let character_id = i32::from_le_bytes([p[76], p[77], p[78], p[79]]);
        let name = Reader::new(&p[80..100]).fixed_str(20).unwrap_or_default();
        // `clIP.siLevel` sits at &clBI + 0x3A4 = 932 (binary-authoritative).
        let level = i16::from_le_bytes([p[932], p[933]]);
        if character_id != 0 {
            occupied += 1;
            println!("probe: slot {slot}: char {character_id} {name:?} level {level}");
        } else {
            println!("probe: slot {slot}: empty");
        }
    }
    println!("probe: character list has {occupied} character(s)");

    // S12/S13: select the first character and drive world entry.
    let mut w = Writer::with_capacity(4);
    w.i32(0);
    send_game_encoded(
        &mut game,
        &mut session_tx,
        GameMsg::RequestSelectchar,
        &w.finish(),
    )
    .await?;

    // SERVERINFO, then RESPONSE_LOGIN carrying the 592-byte clMakeCharInfo.
    let (cmd, plain) = read_game_frame(&mut game, &mut gparser, &mut gbuf, &mut link_rx).await?;
    expect_game_cmd(cmd, GameMsg::ResponseServerinfo)?;
    let service_area = i16::from_le_bytes([plain[HEADER_SIZE], plain[HEADER_SIZE + 1]]);
    println!(
        "probe: serverinfo ({} bytes) service_area={service_area}",
        plain.len() - HEADER_SIZE
    );

    let (cmd, plain) = read_game_frame(&mut game, &mut gparser, &mut gbuf, &mut link_rx).await?;
    expect_game_cmd(cmd, GameMsg::ResponseLogin)?;
    let p = &plain[HEADER_SIZE..];
    if p.len() != MAKE_CHAR_INFO_PAYLOAD_LEN {
        return Err(format!(
            "response-login body is {} bytes, expected {MAKE_CHAR_INFO_PAYLOAD_LEN}",
            p.len()
        )
        .into());
    }
    let type_kind = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
    let make_char_unique = u16::from_le_bytes([p[4], p[5]]);
    let level = p[6];
    let life_x = u32::from_le_bytes([p[8], p[9], p[10], p[11]]);
    let y_map_speed = u32::from_le_bytes([p[12], p[13], p[14], p[15]]);
    let life = life_x & 0xF_FFFF;
    let x = (life_x >> 20) & 0x3FF;
    let y = y_map_speed & 0x3FF;
    let map_index = (y_map_speed >> 10) & 0x3FF;
    println!(
        "probe: login char unique={make_char_unique} kind={} level={level} life={life} map={map_index} ({x},{y})",
        type_kind >> 1
    );
    if u16::try_from(char_unique).unwrap_or(0) != make_char_unique {
        return Err("make-char unique does not match the session charunique".into());
    }
    // `clOrder.siOrder` must be ORDER_STOP (2), or the client's SetChar hits
    // the `default:` branch and the char's order state stays unset.
    if p[0x14] != 2 {
        return Err(format!("login packet clOrder.siOrder is {}, want 2", p[0x14]).into());
    }

    // The dev character sits in Village01 (map index 1), whose client-side
    // `siVillageUnique` is 1; the client blocks movement until this reply.
    let mut w = Writer::with_capacity(4);
    w.i32(1);
    send_game_encoded(
        &mut game,
        &mut session_tx,
        GameMsg::RequestVillagestruct,
        &w.finish(),
    )
    .await?;
    let (_cmd, plain) = read_until_game_cmd(
        &mut game,
        &mut gparser,
        &mut gbuf,
        &mut link_rx,
        GameMsg::ResponseVillagestruct,
    )
    .await?;
    let village = i16::from_le_bytes([plain[HEADER_SIZE], plain[HEADER_SIZE + 1]]);
    println!(
        "probe: village {village} structure ({} bytes) received",
        plain.len() - HEADER_SIZE
    );

    if let Some(spec) = move_item {
        probe_item_move(
            &mut game,
            &mut gparser,
            &mut gbuf,
            &mut link_rx,
            &mut session_tx,
            spec,
        )
        .await?;
    }

    if let Some(spec) = drop_item {
        probe_item_drop(
            &mut game,
            &mut gparser,
            &mut gbuf,
            &mut link_rx,
            &mut session_tx,
            spec,
        )
        .await?;
    }

    if let Some(spec) = use_bonus {
        probe_use_bonus(
            &mut game,
            &mut gparser,
            &mut gbuf,
            &mut link_rx,
            &mut session_tx,
            char_unique,
            spec,
        )
        .await?;
    }

    // Take ownership of this session's socket so the observer probe can borrow it
    // and send the move on the connection that is already in the world.
    let mut mover = Session::adopt(
        Wire {
            stream: game,
            parser: gparser,
            buf: gbuf.to_vec(),
            link_rx,
            session_tx,
        },
        Who {
            char_unique,
            spawn_x: x as i32,
            spawn_y: i32::try_from(y).unwrap_or(0),
            spawn_map: (y_map_speed >> 10) & 0x3FF,
        },
    );

    if let Some(observer) = observe_char_update {
        probe_observer_fan(&addr, &mut mover, observer).await?;
    }

    println!("probe: front + game login path OK; world entry OK");
    Ok(())
}

/// How much of `deadline` is left, floored at a millisecond so a per-read timeout
/// is never zero (`tokio::time::timeout` with a zero duration always expires).
fn remaining(deadline: std::time::Instant) -> std::time::Duration {
    deadline
        .saturating_duration_since(std::time::Instant::now())
        .max(std::time::Duration::from_millis(1))
}

/// Walks a second account all the way into the world, then asserts the 200 fan
/// (slice 6 B1) reaches it.
///
/// Both characters spawn on the palace tile — `0009_seed_second_account.sql` puts
/// `Tester2` on `Tester`'s cell deliberately — so the observer is inside the mover's
/// 3x3 near cell and the fan can reach it.
///
/// # What is asserted
///
/// Two things, and the **order matters** — one without the other proves nothing:
///
/// 1. **A `RESPONSE_LOGINMANY` (6) naming the mover, delivered *before* the walk.**
///    This is the frame that makes another player exist: the client `PushChar`es
///    each record (`Client-Login.cpp:53-85`) and `SetCharReservedChar` turns that
///    queue into avatars (`Client-SetChar.cpp:112-138`). Without it there is no
///    `CR[id]` for the mover and **every** later frame about them is discarded at
///    `Client-Person-BigInfo.cpp:1166-1169`.
/// 2. **`RESPONSE_CHARUPDATE` (200) frames naming the mover**, with `siX`/`siY`
///    changing as it walks.
///
/// An earlier version of this probe asserted only the 200s. It **passed** while the
/// GUI showed no avatar at all: the fan was arriving correctly and the client was
/// throwing it away, because it had never been told the other player existed. A
/// socket-level assertion sits below exactly the layer that discarded them, so the
/// 6 is now checked first — otherwise the same blind spot returns.
///
/// It deliberately does *not* look for a health bar. The client draws the floating
/// gauge for `CR[1]` only (`NCharPerson.cpp:3920`), so no bystander ever sees one;
/// the fan is for position and walk animation.
///
/// The mover's socket is borrowed so the 240 goes out on the connection that is
/// already in the world, rather than a third login.
async fn probe_observer_fan(
    front_addr: &str,
    mover: &mut Session,
    (observer_user, observer_password): (String, String),
) -> Result<(), AnyError> {
    let mover_id = mover.char_unique;
    let mover_map = mover.spawn_map;
    let mut observer = Session::enter(front_addr, &observer_user, &observer_password).await?;
    println!(
        "probe: observer {observer_user:?} in the world as char_unique {} (mover is {mover_id})",
        observer.char_unique
    );
    if observer.char_unique == mover_id {
        return Err(format!(
            "observer and mover share char_unique {mover_id}: the client treats the other's \
             position updates as its own (bselfswitch) and would ignore them. The server \
             allocates one id per session, so this means two sessions were bound to one \
             character"
        )
        .into());
    }

    // Close the gap first. The two characters' positions persist between probe runs,
    // so after a few of them the mover has walked out of the observer's 3x3 block and
    // no announcement reaches it. Walking the observer onto the mover's cell makes
    // the run self-contained instead of dependent on leftover state — and it also
    // exercises the cell-crossing announcement, which is how the legacy makes a
    // character appear to somebody who walks up to them (`m_pNewCell`,
    // `ServerMap.cpp:73-150`).
    if !observer.is_near(mover.spawn_x, mover.spawn_y) {
        println!(
            "probe: observer at ({}, {}) is not in the mover's near block; walking it to ({}, {})",
            observer.spawn_x, observer.spawn_y, mover.spawn_x, mover.spawn_y
        );
        observer.send_move((mover.spawn_x, mover.spawn_y)).await?;
        // One tile per `5000 / move_speed` ms; a cell is 10x15 tiles.
        tokio::time::sleep(std::time::Duration::from_millis(9_000)).await;
    }

    // (1) The observer must know the mover exists. Either the announcement arrived at
    //     entry (they spawned together) or it arrived because the observer just
    //     crossed into the mover's cell.
    let announced = observer
        .await_login_many_naming(mover_id, std::time::Duration::from_secs(10))
        .await?
        .ok_or_else(|| {
            format!(
                "observer {observer_user:?} never received a LOGINMANY naming the mover \
                 (char_unique {mover_id}): the other player does not exist on this client, \
                 so its 200 updates would be discarded (CharUniqueInfo is 0). This is the \
                 frame that makes an avatar appear"
            )
        })?;
    println!(
        "probe: observer was told about the mover — kind={} level={} life={} at ({}, {}) map {}",
        announced.make_char_kind,
        announced.level,
        announced.life,
        announced.x,
        announced.y,
        announced.map_index
    );
    if announced.map_index != (mover_map & 0x3FF) as u16 {
        return Err(format!(
            "the announced record names map {} but the observer's client holds map {} — \
             SetCharReservedChar skips a record whose uiMapIndex differs, so the avatar \
             would never be built",
            announced.map_index, mover_map
        )
        .into());
    }

    // (2) Three tiles along x: a 3-tile walk commits one tile per
    // `5000 / move_speed` ms, so at the base speed of 10 that is ~500 ms per tile and
    // three distinct cells are comfortably inside the deadline.
    let dest = (mover.spawn_x + 3, mover.spawn_y);
    println!(
        "probe: mover walking ({}, {}) -> {dest:?}",
        mover.spawn_x, mover.spawn_y
    );
    mover.send_move(dest).await?;

    let mut seen: Vec<(i16, i16)> = Vec::new();
    let mut bystander_frames = 0usize;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline && seen.len() < 3 {
        // A per-read timeout, not just a loop deadline: without one the loop blocks
        // forever on a silent socket instead of reporting that the fan went quiet.
        let Some((cmd, plain)) = observer.read_frame_within(remaining(deadline)).await? else {
            break;
        };
        if cmd != GameMsg::ResponseCharupdate as u16 {
            continue;
        }
        let update = BasicCharUpdate::decode(&plain[HEADER_SIZE..])?;
        // A 200 naming somebody *else* is expected, not an error: the near block is
        // shared, so any third character standing nearby fans here too. Only the
        // mover's own frames are what this probe is asserting on. (An earlier
        // version failed the probe on exactly this, and it fired against a leftover
        // GUI client walking around the palace — correct server behaviour, wrong
        // assertion.)
        if i32::from(update.si_char_unique) != mover_id {
            bystander_frames += 1;
            continue;
        }
        let cell = (update.si_x, update.si_y);
        if seen.last() != Some(&cell) {
            seen.push(cell);
        }
    }

    if bystander_frames > 0 {
        println!(
            "probe: observer also received {bystander_frames} 200 frame(s) from other characters \
             in the near block (expected -- the fan is not mover-exclusive)"
        );
    }
    if seen.is_empty() {
        return Err(format!(
            "observer {observer_user:?} received no 200 for the mover's walk: the fan did not \
             reach it. Is the observer inside the mover's 3x3 cell block?"
        )
        .into());
    }
    println!(
        "probe: observer saw the mover at {} distinct cell(s): {seen:?}",
        seen.len()
    );
    if seen.len() < 2 {
        return Err(format!(
            "observer saw only one cell ({seen:?}); the fan is not tracking the walk tile by tile"
        )
        .into());
    }
    println!("probe: the 200 fan reached a second, distinct session over a real socket");
    Ok(())
}

/// One account's live game session: char select, world entry, and the ciphers and
/// framing needed to keep reading frames off it.
///
/// The `--observe-char-update` probe needs a *second* one of these, built by
/// [`Session::enter`] — the same path the mover walked, factored out so the two
/// cannot drift. The primary session adopts its already-open socket via
/// [`Session::adopt`] rather than logging in twice.
struct Session {
    stream: TcpStream,
    parser: FrameParser,
    buf: Vec<u8>,
    /// Server->client is the fixed link cipher; client->server is the session cipher
    /// keyed by the `RESPONSE_CLIENTCHARUNIQUE` pair.
    link_rx: Cipher,
    session_tx: Cipher,
    /// This session's `char_unique`, as handed out in `RESPONSE_CLIENTCHARUNIQUE`.
    char_unique: i32,
    /// Where the character was placed on entry, from `RESPONSE_LOGIN`'s `clPos`.
    spawn_x: i32,
    spawn_y: i32,
    /// The map from the same frame's `uiMapIndex`, needed to check that a record
    /// naming this character is on a map the client already holds.
    spawn_map: u32,
    /// LOGINMANY records seen while entering, which must not be thrown away.
    ///
    /// The announcement about the other characters is sent *with* the entry
    /// sequence, so it can arrive before the village-struct reply that `enter`
    /// waits for. An earlier version dropped every frame that was not the awaited
    /// one, which made the observer probe wait for a 6 that had already gone past —
    /// a race that showed up as an intermittent hang, which is worse than a failure.
    seen_login_many: Vec<MakeCharInfo>,
}

/// The socket half of a [`Session`]: how its bytes are framed and deciphered.
struct Wire {
    stream: TcpStream,
    parser: FrameParser,
    buf: Vec<u8>,
    link_rx: Cipher,
    session_tx: Cipher,
}

/// What the server said about who a [`Session`] is.
struct Who {
    /// The session's `char_unique`, from `RESPONSE_CLIENTCHARUNIQUE`.
    char_unique: i32,
    /// The entry cell, from `RESPONSE_LOGIN`'s `clPos`.
    spawn_x: i32,
    spawn_y: i32,
    /// The entry map, from the same frame's `uiMapIndex`.
    spawn_map: u32,
}

impl Session {
    /// Takes ownership of a socket that has already completed world entry.
    ///
    /// The wire half and the character facts are grouped because they are two
    /// different things being joined: one is how the bytes are read, the other is
    /// what the server said about who this session is.
    fn adopt(wire: Wire, who: Who) -> Self {
        Self {
            stream: wire.stream,
            parser: wire.parser,
            buf: wire.buf,
            link_rx: wire.link_rx,
            session_tx: wire.session_tx,
            char_unique: who.char_unique,
            spawn_x: who.spawn_x,
            spawn_y: who.spawn_y,
            spawn_map: who.spawn_map,
            seen_login_many: Vec::new(),
        }
    }

    /// Whether `spawn` is inside this character's 3×3 near cell, using the same
    /// 10×15-tile cells the server's fan uses.
    ///
    /// The observer cannot *receive* a 6 about somebody it is not near — the
    /// server's announcement is itself a near-block query — so the probe has to know
    /// whether it is in range before it asserts anything about the fan.
    fn is_near(&self, x: i32, y: i32) -> bool {
        let cell = |(x, y): (i32, i32)| (x.div_euclid(10), y.div_euclid(15));
        let (ox, oy) = cell((self.spawn_x, self.spawn_y));
        let (ax, ay) = cell((x, y));
        (ox - ax).abs() <= 1 && (oy - ay).abs() <= 1
    }

    /// Records every LOGINMANY in `payload`, returning whether one named
    /// `char_unique` — which is also appended to [`Self::seen_login_many`].
    ///
    /// Records are packed with no length prefix, so the body is a whole number of
    /// 592-byte records and the whole body is walked.
    fn take_login_many(&mut self, cmd: u16, payload: &[u8], char_unique: i32) -> bool {
        if cmd != GameMsg::ResponseLoginmany as u16 {
            return false;
        }
        let body = &payload[HEADER_SIZE..];
        let mut found = false;
        for record in body.chunks(MAKE_CHAR_INFO_PAYLOAD_LEN) {
            let Ok(info) = MakeCharInfo::decode(record) else {
                continue;
            };
            if i32::from(info.make_char_unique) == char_unique {
                found = true;
            }
            self.seen_login_many.push(info);
        }
        found
    }

    /// Logs a fresh account in and puts its character in the world.
    ///
    /// Steps S1-S13 of the entry path: front secret key, English login, notice,
    /// world list, reconnect to the game server, `CLIENTCHARUNIQUE`, game login, the
    /// 15 char-list frames, char select, `SERVERINFO`, `RESPONSE_LOGIN`, and the
    /// village-struct round trip the client blocks movement on.
    async fn enter(front_addr: &str, user: &str, password: &str) -> Result<Self, AnyError> {
        println!("probe: entering as {user:?}");
        let mut stream = TcpStream::connect(front_addr).await?;
        let mut parser = FrameParser::new();
        let mut buf = [0u8; 4096];

        // S1: secret key, unprompted and in the clear.
        let frame = read_frame(&mut stream, &mut parser, &mut buf).await?;
        expect_cmd(&frame, FrontMsg::ResponseSecretkey)?;
        let key1 = frame.payload()[2];
        let key2 = frame.payload()[4];
        let mut cipher = Cipher::session_cipher(key1, key2);

        // S2: English login (21), ciphered.
        send_encoded(
            &mut stream,
            &mut cipher,
            FrontMsg::RequestEnglishLogin,
            &english_login(user, password),
        )
        .await?;
        let frame = read_frame(&mut stream, &mut parser, &mut buf).await?;
        expect_cmd(&frame, FrontMsg::ResponseLogin)?;
        let mut r = Reader::new(frame.payload());
        let result = r.u16()?;
        let auth_key = r.i32()?;
        if result != 0 {
            return Err(format!("login failed with result {result}").into());
        }

        // S5: notice, then S6: world list.
        send_encoded(&mut stream, &mut cipher, FrontMsg::RequestNotice, &[]).await?;
        let frame = read_frame(&mut stream, &mut parser, &mut buf).await?;
        expect_cmd(&frame, FrontMsg::ResponseNotice)?;

        let mut w = Writer::new();
        w.fixed_str(user, 20);
        send_encoded(
            &mut stream,
            &mut cipher,
            FrontMsg::RequestWorldlist,
            &w.finish(),
        )
        .await?;
        let frame = read_frame(&mut stream, &mut parser, &mut buf).await?;
        expect_cmd(&frame, FrontMsg::ResponseWorldlist)?;
        let payload = frame.payload();
        let count = u16::from_le_bytes([payload[0], payload[1]]);
        if count == 0 {
            return Err("world list was empty".into());
        }
        let entry = &payload[2..];
        let ip = Reader::new(&entry[28..44])
            .fixed_str(16)
            .unwrap_or_default();
        let port = u16::from_le_bytes([entry[44], entry[45]]);

        // S7: reconnect to the game server the world list advertised.
        let mut stream = TcpStream::connect(format!("{ip}:{port}")).await?;
        let mut parser = FrameParser::new();
        let mut buf = vec![0u8; 8192];
        let mut link_rx = Cipher::link_cipher();

        // S8: CLIENTCHARUNIQUE.
        let (cmd, plain) =
            read_game_frame(&mut stream, &mut parser, &mut buf, &mut link_rx).await?;
        expect_game_cmd(cmd, GameMsg::ResponseClientcharunique)?;
        let mut r = Reader::new(&plain[HEADER_SIZE..]);
        let char_unique = r.i32()?;
        let gkey1 = r.u8()?;
        let gkey2 = r.u8()?;
        let gversion = r.i16()?;
        if gversion > 1 {
            return Err(format!("server advertises version {gversion}; client is 1").into());
        }
        let mut session_tx = Cipher::session_cipher(gkey1, gkey2);

        // S9: game login.
        let mut w = Writer::with_capacity(48);
        w.i32(auth_key);
        w.fixed_str(user, 20);
        w.fixed_str(password, 20);
        w.u8(0);
        w.pad(3);
        send_game_encoded(
            &mut stream,
            &mut session_tx,
            GameMsg::RequestLogin,
            &w.finish(),
        )
        .await?;

        // S10/S11: one char-list packet per slot; the client wants all 15.
        for _ in 0..15 {
            let (cmd, _plain) =
                read_game_frame(&mut stream, &mut parser, &mut buf, &mut link_rx).await?;
            expect_game_cmd(cmd, GameMsg::ResponseCharlist)?;
        }

        // S12: select slot 0, then SERVERINFO and RESPONSE_LOGIN.
        let mut w = Writer::with_capacity(4);
        w.i32(0);
        send_game_encoded(
            &mut stream,
            &mut session_tx,
            GameMsg::RequestSelectchar,
            &w.finish(),
        )
        .await?;
        let (cmd, _plain) =
            read_game_frame(&mut stream, &mut parser, &mut buf, &mut link_rx).await?;
        expect_game_cmd(cmd, GameMsg::ResponseServerinfo)?;
        let (cmd, plain) =
            read_game_frame(&mut stream, &mut parser, &mut buf, &mut link_rx).await?;
        expect_game_cmd(cmd, GameMsg::ResponseLogin)?;
        let p = &plain[HEADER_SIZE..];
        if p.len() != MAKE_CHAR_INFO_PAYLOAD_LEN {
            return Err(format!(
                "response-login body is {} bytes, expected {MAKE_CHAR_INFO_PAYLOAD_LEN}",
                p.len()
            )
            .into());
        }
        let life_x = u32::from_le_bytes([p[8], p[9], p[10], p[11]]);
        let y_map_speed = u32::from_le_bytes([p[12], p[13], p[14], p[15]]);
        let x = ((life_x >> 20) & 0x3FF) as i32;
        let y = (y_map_speed & 0x3FF) as i32;
        let spawn_map = (y_map_speed >> 10) & 0x3FF;
        println!(
            "probe: {user:?} in the world at ({x}, {y}) map {spawn_map} as char_unique {char_unique}"
        );

        // S13: the village-struct round trip the client blocks movement on. Any
        // LOGINMANY that arrives while waiting is kept, not dropped — it is the
        // announcement about who else is in the world, and it usually arrives here.
        let mut seen_login_many: Vec<MakeCharInfo> = Vec::new();
        let mut w = Writer::with_capacity(4);
        w.i32(1);
        send_game_encoded(
            &mut stream,
            &mut session_tx,
            GameMsg::RequestVillagestruct,
            &w.finish(),
        )
        .await?;
        loop {
            let (cmd, plain) =
                read_game_frame(&mut stream, &mut parser, &mut buf, &mut link_rx).await?;
            if cmd == GameMsg::ResponseVillagestruct as u16 {
                break;
            }
            if cmd != GameMsg::ResponseLoginmany as u16 {
                continue;
            }
            for record in plain[HEADER_SIZE..].chunks(MAKE_CHAR_INFO_PAYLOAD_LEN) {
                if let Ok(info) = MakeCharInfo::decode(record) {
                    seen_login_many.push(info);
                }
            }
        }

        Ok(Self::adopt(
            Wire {
                stream,
                parser,
                buf,
                link_rx,
                session_tx,
            },
            Who {
                char_unique,
                spawn_x: x,
                spawn_y: y,
                spawn_map,
            },
        ))
        .map(|mut s| {
            s.seen_login_many = seen_login_many;
            s
        })
    }

    /// The next game frame, or `None` at end of stream **or if `within` elapses**.
    ///
    /// The timeout is not optional. A probe that reads until a deadline checked
    /// only *between* frames blocks forever on a silent socket, because there is no
    /// frame to return to and re-check it — which is how an earlier version of the
    /// observer probe hung instead of failing.
    async fn read_frame_within(
        &mut self,
        within: std::time::Duration,
    ) -> Result<Option<(u16, Vec<u8>)>, AnyError> {
        match tokio::time::timeout(within, self.read_frame()).await {
            Ok(result) => result,
            Err(_) => Ok(None),
        }
    }

    /// The next game frame, or `None` at end of stream.
    async fn read_frame(&mut self) -> Result<Option<(u16, Vec<u8>)>, AnyError> {
        match read_frame(&mut self.stream, &mut self.parser, &mut self.buf).await {
            Ok(frame) => {
                let wire = frame.as_bytes();
                let mut plain = vec![0u8; wire.len()];
                if !self.link_rx.decode(wire, &mut plain)? {
                    return Err("game link cipher integrity mismatch".into());
                }
                Ok(Some((u16::from_le_bytes([plain[3], plain[4]]), plain)))
            }
            Err(e) if e.to_string().contains("closed") => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Waits for a `RESPONSE_LOGINMANY` (6) record naming `char_unique`, or `None`
    /// if the deadline passes first.
    ///
    /// Records are packed with no length prefix, so a body is a whole number of
    /// 592-byte records and the whole body is walked looking for the id. Returns the
    /// decoded record so the caller can check the fields the client actually reads
    /// before it builds an avatar from it.
    ///
    /// Reads past the target are *not* buffered — anything else on the socket is
    /// dropped, which is acceptable here because this runs immediately after entry,
    /// when the only thing in flight is the announcement itself.
    async fn await_login_many_naming(
        &mut self,
        char_unique: i32,
        within: std::time::Duration,
    ) -> Result<Option<MakeCharInfo>, AnyError> {
        // Already seen while entering? That is the common case: the announcement
        // travels with the entry sequence and usually beats the village reply.
        if let Some(hit) = self
            .seen_login_many
            .iter()
            .find(|i| i32::from(i.make_char_unique) == char_unique)
        {
            return Ok(Some(hit.clone()));
        }
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline {
            let Some((cmd, plain)) = self.read_frame_within(remaining(deadline)).await? else {
                return Ok(None);
            };
            if self.take_login_many(cmd, &plain, char_unique) {
                return Ok(self
                    .seen_login_many
                    .iter()
                    .find(|i| i32::from(i.make_char_unique) == char_unique)
                    .cloned());
            }
        }
        Ok(None)
    }

    /// Sends `REQUEST_USERORDER` (240) with an `ORDER_MOVE` to `dest`.
    ///
    /// Built through [`OrderData::move_order`] rather than by hand: the wire layout
    /// is 24 bytes with three pad slots, and a hand-written `Writer` produced a
    /// 32-byte body that decoded to `si_order = 0` and was dropped by the server
    /// as an unmodelled order. The probe must speak the same body the GUI does.
    async fn send_move(&mut self, dest: (i32, i32)) -> Result<(), AnyError> {
        let order = OrderData::move_order(
            self.char_unique,
            i16::try_from(dest.0).unwrap_or(0),
            i16::try_from(dest.1).unwrap_or(0),
        );
        send_game_encoded(
            &mut self.stream,
            &mut self.session_tx,
            GameMsg::RequestUserorder,
            &order.encode(),
        )
        .await
    }
}

/// Runs the inventory round trip: read the item array the server generated, move
/// one slot, read it again.
///
/// There is no reply to `REQUEST_ITEMMOVE` in the legacy opcode table, so the
/// only way to observe the effect is to ask for the character info again — which
/// is exactly what the GUI client does, and exactly what makes the mutation
/// visible at all.
async fn probe_item_move(
    game: &mut TcpStream,
    gparser: &mut FrameParser,
    gbuf: &mut [u8],
    link_rx: &mut Cipher,
    session_tx: &mut Cipher,
    (from, to): (u16, u16),
) -> Result<(), AnyError> {
    let before = fetch_item_array(game, gparser, gbuf, link_rx, session_tx).await?;
    let from_item = before[usize::from(from)];
    let to_item = before[usize::from(to)];
    println!(
        "probe: before move — slot {from} holds item {} x{}, slot {to} holds item {} x{}",
        from_item.unique(),
        from_item.count(),
        to_item.unique(),
        to_item.count()
    );
    if from_item.is_empty() {
        return Err(format!("slot {from} is empty, nothing to move").into());
    }

    let mut w = Writer::with_capacity(4);
    w.u16(from);
    w.u16(to);
    send_game_encoded(game, session_tx, GameMsg::RequestItemmove, &w.finish()).await?;
    println!("probe: sent REQUEST_ITEMMOVE {from} -> {to}");

    let after = fetch_item_array(game, gparser, gbuf, link_rx, session_tx).await?;
    let from_after = after[usize::from(from)];
    let to_after = after[usize::from(to)];
    println!(
        "probe: after move  — slot {from} holds item {} x{}, slot {to} holds item {} x{}",
        from_after.unique(),
        from_after.count(),
        to_after.unique(),
        to_after.count()
    );

    // Either the two records exchanged places, or — when they held the same item
    // — they merged into one stack in the destination and the source emptied.
    let swapped = from_after == to_item && to_after == from_item;
    let stacked = from_after.is_empty()
        && to_after.unique() == from_item.unique()
        && to_after.count() == from_item.count() + to_item.count();
    if swapped || stacked {
        println!(
            "probe: the move round-tripped through PostgreSQL ({})",
            if swapped { "swapped" } else { "stacked" }
        );
        Ok(())
    } else {
        Err(format!(
            "after moving {from} -> {to}, the server's array does not agree with the move: \
             slot {from} is {} x{} and slot {to} is {} x{}, expected a swap or a stack",
            from_after.unique(),
            from_after.count(),
            to_after.unique(),
            to_after.count()
        )
        .into())
    }
}

/// Runs the drop round trip: read the item array, drop part of one stack, and
/// check both the 7210 the server replies with and the persisted array.
///
/// A drop is the first inventory operation with an actual acknowledgement. The
/// server answers `GAMEMSG_REQUEST_INVDROP` (307) with
/// `GAMEMSG_RESPONSE_PERSONITEMINFO` (7210) carrying the slot's new contents —
/// not with 309, which only draws a ground icon. The GUI client never updates its
/// bag optimistically on a drop, so this frame is the whole confirmation.
async fn probe_item_drop(
    game: &mut TcpStream,
    gparser: &mut FrameParser,
    gbuf: &mut [u8],
    link_rx: &mut Cipher,
    session_tx: &mut Cipher,
    (slot, count, claim): (u16, u16, Option<u16>),
) -> Result<(), AnyError> {
    let before = fetch_item_array(game, gparser, gbuf, link_rx, session_tx).await?;
    let held = before[usize::from(slot)];
    println!(
        "probe: before drop — slot {slot} holds item {} x{}",
        held.unique(),
        held.count()
    );
    if held.is_empty() {
        return Err(format!("slot {slot} is empty, nothing to drop").into());
    }
    if count == 0 || count > held.count() {
        return Err(format!("cannot drop {count} of the {} in slot {slot}", held.count()).into());
    }

    // `cltGameMsgRequest_InvDrop` is `SI32 siItemPos` then the client's copy of
    // the record, carrying the count it wants removed. A `claim` overrides the
    // unique so the request deliberately disagrees with the server.
    let mut w = Writer::with_capacity(4 + CLT_ITEM_SIZE);
    w.i32(i32::from(slot));
    let mut claimed = held;
    claimed.set_count(count);
    if let Some(lying) = claim {
        claimed = Item::from_unique(lying, count);
        println!(
            "probe: deliberately claiming item {lying} for a slot holding {}",
            held.unique()
        );
    }
    w.bytes(&claimed.encode());
    send_game_encoded(game, session_tx, GameMsg::RequestInvdrop, &w.finish()).await?;
    println!("probe: sent REQUEST_INVDROP slot {slot} count {count}");

    // The 7210 is the first frame the server volunteers, so read it directly.
    let (cmd, plain) = read_until_game_cmd(
        game,
        gparser,
        gbuf,
        link_rx,
        GameMsg::ResponsePersoniteminfo,
    )
    .await?;
    assert_eq!(cmd, GameMsg::ResponsePersoniteminfo as u16);
    let body = &plain[HEADER_SIZE..];
    println!("probe: got RESPONSE_PERSONITEMINFO ({} bytes)", body.len());

    let echoed_pos = u16::from_le_bytes([body[0], body[1]]);
    let echoed = Item::decode(&body[12..])?;
    let expected_count = held.count() - count;
    println!(
        "probe: 7210 says slot {echoed_pos} now holds item {} x{}",
        echoed.unique(),
        echoed.count()
    );

    if echoed_pos != slot {
        return Err(format!("7210 names slot {echoed_pos}, asked about {slot}").into());
    }

    let remaining_ok = if expected_count == 0 {
        echoed.is_empty()
    } else {
        echoed.unique() == held.unique() && echoed.count() == expected_count
    };

    // A request that disagreed with the stored record must be refused outright:
    // the slot keeps its contents and nothing is written to the ground.
    if claim.is_some() {
        if echoed != held {
            return Err(format!(
                "a request claiming the wrong item was not refused: slot {slot} came back as \
                 item {} x{}, expected the unchanged item {} x{}",
                echoed.unique(),
                echoed.count(),
                held.unique(),
                held.count()
            )
            .into());
        }
        println!("probe: the mismatched request was refused and the slot left intact");
        return Ok(());
    }

    if !remaining_ok {
        return Err(format!(
            "7210 reports slot {slot} as item {} x{}, expected item {} x{}",
            echoed.unique(),
            echoed.count(),
            held.unique(),
            expected_count
        )
        .into());
    }

    // And confirm it reached PostgreSQL rather than just the reply path.
    let after = fetch_item_array(game, gparser, gbuf, link_rx, session_tx).await?;
    let persisted = after[usize::from(slot)];
    if persisted != echoed {
        return Err(format!(
            "the stored array disagrees with the 7210 it just sent: slot {slot} is {} x{} \
             in RESPONSE_ALLCHARINFOTWO but {} x{} in 7210",
            persisted.unique(),
            persisted.count(),
            echoed.unique(),
            echoed.count()
        )
        .into());
    }

    println!("probe: the drop round-tripped through PostgreSQL (7210 confirmed)");
    Ok(())
}

/// Runs the stat-allocation round trip: send `REQUEST_USEBONUS` (250) and check
/// the `RESPONSE_PERSON_IP` (215) the server answers with.
///
/// 215 is the frame the GUI status window applies (`Client-Person-BigInfo.cpp:
/// 236-250`), so asserting it is asserting what the player's screen updates to:
/// `siCharUnique` echoes the session, and the 64-byte `clIP` block at offset 8
/// carries the accepted ability, the decremented bonus and the maxima the server
/// recomputed from the formulas. The assertions assume a fresh dev seed and a
/// single spend; `expected stat = seed + amount` is only valid on a full
/// `scripts\db-init.ps1` apply.
async fn probe_use_bonus(
    game: &mut TcpStream,
    gparser: &mut FrameParser,
    gbuf: &mut [u8],
    link_rx: &mut Cipher,
    session_tx: &mut Cipher,
    char_unique: i32,
    (pos, amount, expect_refused): (i32, i16, bool),
) -> Result<(), AnyError> {
    // `cltGameMsgResponse_UseBonus` (`MsgType-Person.h:1062-1074`), 8 bytes /Zp8:
    // SI32 siPos, SI16 siAmount, 2 pad bytes — the client's `cltMsg` frames the
    // body as `sizeof(clinfo)`, so the declared size on the wire is 5 + 8 = 13.
    let mut w = Writer::with_capacity(8);
    w.i32(pos);
    w.i16(amount);
    w.pad(2);
    send_game_encoded(game, session_tx, GameMsg::RequestUsebonus, &w.finish()).await?;
    println!("probe: sent REQUEST_USEBONUS pos {pos} amount {amount}");

    if expect_refused {
        // A refused spend answers nothing and must not drop the session: prove
        // aliveness with a REQUEST_VILLAGESTRUCT (1101) round trip and fail if a
        // 215 turns up first. (230 can't be the aliveness probe: under
        // `GAME_WORLD_BULK=at-selectchar` every 230 is legacy CHANGESTATUSINFO,
        // whose 20-byte change block a 4-byte body cannot satisfy.)
        let mut w = Writer::with_capacity(4);
        w.i32(1);
        send_game_encoded(game, session_tx, GameMsg::RequestVillagestruct, &w.finish()).await?;
        loop {
            let (cmd, plain) = read_game_frame(game, gparser, gbuf, link_rx).await?;
            if cmd == GameMsg::ResponsePersonIp as u16 {
                return Err("expected the allocation to be refused, but a 215 arrived".into());
            }
            if cmd == GameMsg::ResponseVillagestruct as u16 {
                println!(
                    "probe: allocation refused and the session stayed alive \
                     (village structure {} bytes replied after the refused spend)",
                    plain.len() - HEADER_SIZE
                );
                return Ok(());
            }
        }
    }

    let (cmd, plain) =
        read_until_game_cmd(game, gparser, gbuf, link_rx, GameMsg::ResponsePersonIp).await?;
    assert_eq!(cmd, GameMsg::ResponsePersonIp as u16);
    let body = &plain[HEADER_SIZE..];
    println!("probe: got RESPONSE_PERSON_IP ({} bytes)", body.len());
    if body.len() != 72 {
        return Err(format!(
            "215 body is {} bytes, expected 72 (8 + 64: SI16 siCharUnique, /Zp8 pad, cltPIPInfo)",
            body.len()
        )
        .into());
    }
    let echoed = i16::from_le_bytes([body[0], body[1]]);
    if i16::try_from(char_unique).map_err(|_| "session char_unique too large for SI16")? != echoed {
        return Err(format!("215 names siCharUnique {echoed}, session is {char_unique}").into());
    }

    // clIP block @ 8. The dev seed's abilities (20/15/20/10/10/10/10), bonus 5,
    // level 13, life 161, mana 40 — the values the repaired 0002 applies.
    let idx = usize::try_from(pos).map_err(|_| "siPos is not a 0..=6 ability index")?;
    if !(0..7).contains(&idx) {
        return Err(format!("siPos {pos} is outside the 0..=6 ability space").into());
    }
    let ability = i32::from_le_bytes(body[8 + idx * 4..8 + idx * 4 + 4].try_into().unwrap());
    let level = i16::from_le_bytes([body[36], body[37]]);
    let bonus = i16::from_le_bytes([body[38], body[39]]);
    let life = i32::from_le_bytes(body[40..44].try_into().unwrap());
    let mana = i32::from_le_bytes(body[44..48].try_into().unwrap());

    let seed = [20, 15, 20, 10, 10, 10, 10];
    let raise = i32::from(amount);
    let expected_ability = seed[idx] + raise;
    // `Parametabox.cpp:3374/3545` — recompute expected maxima from the raised
    // ability; untouched stats keep the seed's stored values.
    let expected_life = if pos == 3 {
        expected_ability * (8000 + 13 * 5) / 1000
    } else {
        161
    };
    let expected_mana = if pos == 2 {
        expected_ability * (4000 + 13 * 5) / 1000
    } else {
        40
    };
    let expected_bonus: i16 = 5 - amount;

    println!(
        "probe: 215 -> ability[{pos}]={ability} level={level} bonus={bonus} life={life} mana={mana}"
    );
    if (ability, level, bonus, life, mana)
        != (
            expected_ability,
            13,
            expected_bonus,
            expected_life,
            expected_mana,
        )
    {
        return Err(format!(
            "215 rejected state: ability[{pos}]={ability} (want {expected_ability}), level={level} \
             (want 13), bonus={bonus} (want {expected_bonus}), life={life} (want {expected_life}), \
             mana={mana} (want {expected_mana})"
        )
        .into());
    }

    println!("probe: the allocation round-tripped and the 215 carries the recomputed stats");
    Ok(())
}

/// Asks for the character info block and returns the item array it carries.
///
/// The reference server answers `REQUEST_CHANGESTATUSINFO` (230) with 25 frames,
/// not with `RESPONSE_ALLCHARINFO` (231); `ALLCHARINFOTWO` (249) is the one that
/// carries `cltPersonInfoTwo`, whose first member is the 80-slot item array.
async fn fetch_item_array(
    game: &mut TcpStream,
    gparser: &mut FrameParser,
    gbuf: &mut [u8],
    link_rx: &mut Cipher,
    session_tx: &mut Cipher,
) -> Result<Vec<gz_proto::item::Item>, AnyError> {
    let mut w = Writer::with_capacity(25);
    w.i32(33);
    send_game_encoded(
        game,
        session_tx,
        GameMsg::RequestChangestatusinfo,
        &w.finish(),
    )
    .await?;

    loop {
        let (cmd, plain) = read_game_frame(game, gparser, gbuf, link_rx).await?;
        if cmd == GameMsg::ResponseAllcharinfotwo as u16 {
            let body = &plain[HEADER_SIZE..];
            println!("probe: ALLCHARINFOTWO body is {} bytes", body.len());
            return gz_proto::item::decode_item_array(body).map_err(Into::into);
        }
    }
}

/// Reads frames until `expected` arrives, reporting what it skipped over.
///
/// World entry is a burst rather than a request/response pair: selecting a
/// character makes the server replay ~45 captured frames before it answers
/// anything, so a probe cannot assume the next frame is the reply it asked for.
async fn read_until_game_cmd(
    game: &mut TcpStream,
    gparser: &mut FrameParser,
    gbuf: &mut [u8],
    link_rx: &mut Cipher,
    expected: GameMsg,
) -> Result<(u16, Vec<u8>), AnyError> {
    let mut skipped = 0usize;
    loop {
        let (cmd, plain) = read_game_frame(game, gparser, gbuf, link_rx).await?;
        if cmd == expected as u16 {
            if skipped > 0 {
                println!("probe: skipped {skipped} world-entry frame(s) looking for {expected:?}");
            }
            return Ok((cmd, plain));
        }
        skipped += 1;
    }
}

/// Builds the 2108-byte `sRequest_English_LogIn` body.
fn english_login(user: &str, password: &str) -> Vec<u8> {
    let mut w = Writer::with_capacity(2108);
    w.fixed_str(user, 20);
    w.fixed_str(password, 20);
    w.fixed_str("00-00-00-00-00-00", 20);
    w.pad(2048);
    w.finish()
}

async fn send_encoded(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    cmd: FrontMsg,
    payload: &[u8],
) -> Result<(), AnyError> {
    let frame = Frame::new(cmd as u16, payload)?;
    let wire = frame.into_bytes();
    let enciphered = cipher.encode(&wire)?;
    stream.write_all(&enciphered).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_frame(
    stream: &mut TcpStream,
    parser: &mut FrameParser,
    buf: &mut [u8],
) -> Result<Frame, AnyError> {
    loop {
        if let Some(frame) = parser.next_frame()? {
            return Ok(frame);
        }
        let n = stream.read(buf).await?;
        if n == 0 {
            return Err("connection closed while waiting for a frame".into());
        }
        parser.feed(&buf[..n])?;
    }
}

fn expect_cmd(frame: &Frame, expected: FrontMsg) -> Result<(), AnyError> {
    if frame.cmd() == expected as u16 {
        Ok(())
    } else {
        Err(format!("expected opcode {}, got {}", expected as u16, frame.cmd()).into())
    }
}

async fn read_game_frame(
    stream: &mut TcpStream,
    parser: &mut FrameParser,
    buf: &mut [u8],
    link: &mut Cipher,
) -> Result<(u16, Vec<u8>), AnyError> {
    let frame = read_frame(stream, parser, buf).await?;
    let wire = frame.as_bytes();
    let mut plain = vec![0u8; wire.len()];
    if !link.decode(wire, &mut plain)? {
        return Err("game link cipher integrity mismatch".into());
    }
    let cmd = u16::from_le_bytes([plain[3], plain[4]]);
    Ok((cmd, plain))
}

async fn send_game_encoded(
    stream: &mut TcpStream,
    cipher: &mut Cipher,
    cmd: GameMsg,
    payload: &[u8],
) -> Result<(), AnyError> {
    let frame = Frame::new(cmd as u16, payload)?;
    let wire = frame.into_bytes();
    let enciphered = cipher.encode(&wire)?;
    stream.write_all(&enciphered).await?;
    stream.flush().await?;
    Ok(())
}

fn expect_game_cmd(cmd: u16, expected: GameMsg) -> Result<(), AnyError> {
    if cmd == expected as u16 {
        Ok(())
    } else {
        Err(format!("expected game opcode {}, got {}", expected as u16, cmd).into())
    }
}
