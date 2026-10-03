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
use gz_db::{
    AccountRepository, CharacterRepository, PgAccountRepository, PgCharacterRepository,
    RepositoryError,
};
use gz_net::{COMPRESSION_FLAG, Frame, FrameParser, HEADER_SIZE, MAX_FRAME_SIZE};
use gz_proto::game::{CharListEntry, CharUnique, GameLogin, decode_select_char};
use gz_proto::opcodes_game::GameMsg;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

type AnyError = Box<dyn std::error::Error + Send + Sync>;

const DEFAULT_DATABASE_URL: &str = "postgres://postgres:Moon@2020@localhost:5432/goonzu";
const DEFAULT_LISTEN: &str = "0.0.0.0:4010";

/// `MAX_TOTAL_CHAR_PER_USER` — the legacy sends one char-list packet for every
/// slot, occupied or not (`ServerDB-Person.cpp:1560-1606`).
const CHAR_SLOTS: i16 = 3;

/// `CLIENT_COMPAT_VERSION` (`char.cpp:5180`). The client quits if this is
/// higher than its own build, so it must not track the real build number.
const CLIENT_COMPAT_VERSION: i16 = 1;

#[tokio::main]
async fn main() -> Result<(), AnyError> {
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string());
    let listen = std::env::var("GAME_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.to_string());

    let accounts = PgAccountRepository::connect(&database_url).await?;
    let characters = PgCharacterRepository::connect(&database_url).await?;
    let listener = TcpListener::bind(&listen).await?;
    println!("game: listening on {listen}");

    loop {
        let (stream, peer) = listener.accept().await?;
        let accounts = accounts.clone();
        let characters = characters.clone();
        let peer = peer.to_string();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, &peer, accounts, characters).await {
                eprintln!("game: {peer}: {e}");
            }
        });
    }
}

async fn handle_client(
    mut stream: TcpStream,
    peer: &str,
    accounts: PgAccountRepository,
    characters: PgCharacterRepository,
) -> Result<(), AnyError> {
    let (key1, key2) = random_key_bytes();

    // S8: the client cannot send anything until it has this, and it decodes it
    // with the fixed link cipher (which it already has).
    let mut outbound = Cipher::link_cipher();
    let char_unique = random_char_unique();
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
                    let login = GameLogin::decode(payload.as_ref())?;
                    handle_game_login(&mut stream, &mut outbound, &accounts, &characters, &login)
                        .await?;
                }
                c if c == GameMsg::RequestSelectchar as u16 => {
                    let slot = decode_select_char(payload.as_ref())?;
                    // World entry (serverinfo + make-char-info + village
                    // structure) is Stage 4; log so the gap is visible.
                    println!("game: {peer}: selectchar slot {slot} (world entry pending)");
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

/// S9–S11: verify the game login and answer with one char-list packet per slot.
async fn handle_game_login(
    stream: &mut TcpStream,
    outbound: &mut Cipher,
    accounts: &PgAccountRepository,
    characters: &PgCharacterRepository,
    login: &GameLogin,
) -> Result<(), AnyError> {
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

    let rows = characters.list_for_account(account.account_id).await?;
    let mut occupied = 0;
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
                account_idnum: account.account_id,
                account_id: account.username.clone(),
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

    println!(
        "game: login ok for {:?} (account {}, auth_key {}): {occupied} character(s)",
        login.userid, account.account_id, login.auth_key
    );
    Ok(())
}

async fn send_encoded(
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

/// Two key bytes for this connection's session cipher. Unpredictability only
/// needs to defeat accidental reuse in development, not an attacker.
fn random_key_bytes() -> (u8, u8) {
    let v = hash_now();
    (v as u8, (v >> 8) as u8)
}

/// A positive session character id.
fn random_char_unique() -> i32 {
    ((hash_now() >> 16) as i32 & 0x7fff_ffff).max(1)
}

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
}
