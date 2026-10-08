//! Front (entry) service — the client's first hop.
//!
//! Reproduces the client-facing half of the legacy `FrontServer` (blueprint 01
//! Part A, 05 §S1–S6) without the rest of the cluster. The legacy front relays
//! login to an `AuthServer`, which queries `GZAccountDB` through an
//! `AccountDBManager`; this port answers straight from PostgreSQL, because the
//! client only ever sees this socket.
//!
//! Wire shape on this link is **asymmetric** (`Client-Winsock.cpp:139-150`,
//! `FrontClientProtocol.cpp:77-92`):
//!
//! - server → client is **plaintext** (the legacy front's session factory is
//!   created with `bEncrypt = false`);
//! - client → server is **ciphered**, keyed by the two bytes we hand out in
//!   `FRONTMSG_RESPONSE_SECRETKEY`.
//!
//! So this service decodes every inbound frame and encodes none outbound. The
//! one legacy exception, `FRONTMSG_REQUEST_PINGTEST`, is checked *before*
//! decode — but the game client never sends it (only the China patcher does),
//! so it can only arrive here as an already-decoded opcode and is ignored.
//!
//! Environment: `DATABASE_URL`, `FRONT_LISTEN`, `GAME_IP`, `GAME_PORT`.

use std::time::{SystemTime, UNIX_EPOCH};

use gz_cipher::Cipher;
use gz_db::password;
use gz_db::{AccountRepository, PgAccountRepository, RepositoryError};
use gz_net::{Frame, FrameParser, HEADER_SIZE};
use gz_proto::front::{self, LoginResponse, SecretKey, WorldInfo, WorldListRequest};
use gz_proto::opcodes_front::FrontMsg;
use gz_proto::wire::Reader;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

type AnyError = Box<dyn std::error::Error + Send + Sync>;

const DEFAULT_DATABASE_URL: &str = "postgres://postgres:Moon@2020@localhost:5432/goonzu";
const DEFAULT_LISTEN: &str = "0.0.0.0:5700";
const DEFAULT_GAME_IP: &str = "127.0.0.1";
const DEFAULT_GAME_PORT: u16 = 4010;
const SERVER_NAME: &str = "GoonZu Test";

/// Where the world list tells the client to reconnect for gameplay.
#[derive(Clone)]
struct GameTarget {
    ip: String,
    port: u16,
}

/// Elapsed-since-process-start stamp, `+mm:ss.mmm`, prefixed to every log line.
///
/// The front half of the instrument the game service carries (handoff
/// 2026-10-08): a gap between two stamps is time *this* process spent, so the
/// ~20 s re-entry delay can be assigned to one side of the wire rather than
/// guessed at.
fn log_stamp() -> String {
    use std::sync::OnceLock;
    use std::time::Instant;
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let ms = ORIGIN.get_or_init(Instant::now).elapsed().as_millis() as u64;
    let secs = ms / 1000;
    format!("+{:>3}:{:02}.{:03}", secs / 60, secs % 60, ms % 1000)
}

/// Wraps one log call in [`log_stamp`], in place of a bare `println!`.
///
/// The bodies name the `std::` forms directly, and this block was inserted
/// after the mechanical `println!` → `logln!` rewrite — so the rewrite can
/// never reach inside these macros and recurse.
macro_rules! logln {
    ($($arg:tt)*) => {
        std::println!("[{}] {}", log_stamp(), std::format_args!($($arg)*))
    };
}

/// [`logln`]'s stderr twin, in place of the bare `eprintln!` the error paths used.
macro_rules! logln_err {
    ($($arg:tt)*) => {
        std::eprintln!("[{}] {}", log_stamp(), std::format_args!($($arg)*))
    };
}

#[tokio::main]
async fn main() -> Result<(), AnyError> {
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string());
    let listen = std::env::var("FRONT_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.to_string());
    let game = GameTarget {
        ip: std::env::var("GAME_IP").unwrap_or_else(|_| DEFAULT_GAME_IP.to_string()),
        port: std::env::var("GAME_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_GAME_PORT),
    };

    let repo = PgAccountRepository::connect(&database_url).await?;
    let listener = TcpListener::bind(&listen).await?;
    logln!(
        "front: listening on {listen} (game at {}:{})",
        game.ip,
        game.port
    );

    loop {
        let (stream, peer) = listener.accept().await?;
        let repo = repo.clone();
        let game = game.clone();
        let peer = peer.to_string();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, &peer, repo, game).await {
                logln_err!("front: {peer}: {e}");
            }
        });
    }
}

/// Serves one client connection: handshake, then request/response until EOF.
async fn handle_client(
    mut stream: TcpStream,
    peer: &str,
    repo: PgAccountRepository,
    game: GameTarget,
) -> Result<(), AnyError> {
    // S1: the client cannot send anything until it has these key bytes.
    let (key1, key2) = random_keys();
    send_frame(
        &mut stream,
        FrontMsg::ResponseSecretkey as u16,
        &SecretKey { key1, key2 }.encode(),
    )
    .await?;

    // Inbound frames are ciphered with the session key (advances once per
    // decode call, so exactly one Cipher for the whole connection).
    let mut cipher = Cipher::session_cipher(key1, key2);
    let mut parser = FrameParser::new();
    let mut buf = [0u8; 4096];

    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            logln!("front: {peer}: disconnected");
            return Ok(());
        }
        parser.feed(&buf[..n])?;

        while let Some(frame) = parser.next_frame()? {
            let wire = frame.as_bytes();
            let mut plain = vec![0u8; wire.len()];
            let integrity = cipher.decode(wire, &mut plain)?;
            if !integrity {
                // Keys have desynced; continuing would only produce garbage.
                logln_err!(
                    "front: {peer}: cipher desync at opcode {:#06x}",
                    frame.cmd()
                );
                return Ok(());
            }

            let cmd = u16::from_le_bytes([plain[3], plain[4]]);
            let payload = &plain[HEADER_SIZE..];
            logln!("front: {peer}: recv opcode {cmd} ({} bytes)", payload.len());

            match cmd {
                c if c == FrontMsg::RequestLogin as u16
                    || c == FrontMsg::RequestEnglishLogin as u16 =>
                {
                    // Opcode 1 is `sRequest_LogIn`, opcode 21 is
                    // `sRequest_English_LogIn` (same userid/password prefix).
                    let (userid, password) = parse_credentials(payload)?;
                    let response = authenticate(&repo, &userid, &password).await;
                    send_frame(
                        &mut stream,
                        FrontMsg::ResponseLogin as u16,
                        &response.encode(),
                    )
                    .await?;
                    logln!(
                        "front: {peer}: login result={} auth_key={} user={userid:?}",
                        response.result,
                        response.auth_key
                    );
                }
                c if c == FrontMsg::RequestNotice as u16 => {
                    send_frame(
                        &mut stream,
                        FrontMsg::ResponseNotice as u16,
                        &front::notice_payload(),
                    )
                    .await?;
                    logln!("front: {peer}: notice sent");
                }
                c if c == FrontMsg::RequestWorldlist as u16 => {
                    let request = WorldListRequest::decode(payload)?;
                    send_frame(
                        &mut stream,
                        FrontMsg::ResponseWorldlist as u16,
                        &front::encode_world_list(&[world_info(&game)]),
                    )
                    .await?;
                    logln!(
                        "front: {peer}: world list sent for {:?}",
                        request.account_id
                    );
                }
                c if c == FrontMsg::Heartbeat as u16 => {
                    // Sent while the world-select dialog is open; nothing to do.
                }
                other => logln!("front: {peer}: unhandled opcode {other}"),
            }
        }
    }
}

/// Reads `userid[20]` and `userpw[20]` — the common prefix of both login structs.
fn parse_credentials(payload: &[u8]) -> Result<(String, String), AnyError> {
    let mut r = Reader::new(payload);
    Ok((r.fixed_str(20)?, r.fixed_str(20)?))
}

/// Verifies credentials against PostgreSQL and builds the 1000 response.
///
/// Result codes (`FrontMsg.h:423`): 0 success, 1 unknown id, 2 wrong password,
/// 4 blocked, 5 database error. (3, "not an adult", has no source here.)
async fn authenticate(repo: &PgAccountRepository, userid: &str, password: &str) -> LoginResponse {
    let base = |result: u16| LoginResponse {
        result,
        auth_key: 0,
        userid: userid.to_string(),
        gm_level: 0,
        block_mode: 0,
        block_time: [0; 5],
        block_reason: String::new(),
        already_made_char: false,
        server_name: SERVER_NAME.to_string(),
        account_status: 0,
        real_age: 0,
    };

    match repo.find_by_username(userid).await {
        Ok(account) => {
            if !password::verify_password(&account.password_hash, password) {
                base(2)
            } else if account.status != 1 {
                LoginResponse {
                    result: 4,
                    block_mode: account.block_mode,
                    block_reason: account.block_reason.unwrap_or_default(),
                    ..base(4)
                }
            } else {
                LoginResponse {
                    result: 0,
                    auth_key: account.account_id,
                    gm_level: account.gm_level as i16,
                    block_mode: account.block_mode,
                    account_status: account.status,
                    ..base(0)
                }
            }
        }
        Err(RepositoryError::NotFound) => base(1),
        Err(e) => {
            logln_err!("front: account lookup failed for {userid:?}: {e}");
            base(5)
        }
    }
}

fn world_info(game: &GameTarget) -> WorldInfo {
    WorldInfo {
        world_id: 1,
        world_name: "Test".to_string(),
        server_id: 1,
        game_server_ip: game.ip.clone(),
        game_server_port: game.port,
        open_date: "2026".to_string(),
        short_message: "Local rewrite test".to_string(),
        state: 0,
        current_connections: 0,
        goonzu_name: "GoonZu Test".to_string(),
        goonzu_text: "goonzu-rs M1".to_string(),
        population: 1,
        current_price: 100,
        npc_rate: 100,
        fixation_money: 0,
    }
}

async fn send_frame(stream: &mut TcpStream, cmd: u16, payload: &[u8]) -> Result<(), AnyError> {
    let frame = Frame::new(cmd, payload)?;
    stream.write_all(frame.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

/// Two key bytes that need only be unpredictable enough to avoid accidental
/// reuse in development; `/dev/urandom`-grade secrecy is not a security goal
/// for the local test cluster.
fn random_keys() -> (u8, u8) {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .hash(&mut hasher);
    std::thread::current().id().hash(&mut hasher);
    let v = hasher.finish();
    (v as u8, (v >> 8) as u8)
}
