//! Scripted protocol client for the M1 login/entry path.
//!
//! Walks the same front-server exchange the real client does (blueprint 05
//! §S1–S6) so the service can be driven end to end without the GUI, and so
//! every step is a deterministic pass/fail instead of "the client looked
//! okay". It is the harness that must go green before `New Era.exe` is
//! pointed at the service.
//!
//! Usage: `protoclient [front_addr] [user] [password]`
//! Defaults: `127.0.0.1:5700 test test`.

use gz_cipher::Cipher;
use gz_net::{Frame, FrameParser, HEADER_SIZE};
use gz_proto::opcodes_front::FrontMsg;
use gz_proto::opcodes_game::GameMsg;
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

    // S12: select the first character. The world-entry response (serverinfo +
    // make-char-info + village structure) is Stage 4, so nothing comes back.
    let mut w = Writer::with_capacity(4);
    w.i32(0);
    send_game_encoded(
        &mut game,
        &mut session_tx,
        GameMsg::RequestSelectchar,
        &w.finish(),
    )
    .await?;
    println!("probe: selectchar sent (world entry pending)");

    println!("probe: front + game login path OK");
    Ok(())
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
