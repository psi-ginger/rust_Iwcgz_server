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
use gz_net::{Frame, FrameParser};
use gz_proto::opcodes_front::FrontMsg;
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

    println!("probe: front path OK");
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
