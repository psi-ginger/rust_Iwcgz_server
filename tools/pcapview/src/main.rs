use std::fs;
use std::path::Path;

use gz_cipher::Cipher;
use gz_lzf as lzf;
use gz_net::COMPRESSION_FLAG;
use gz_proto::opcodes_front::name as front_name;
use gz_proto::opcodes_game;

const _PLAYER: () = ();

/// Split a length-prefixed stream of ITZ frames into their raw bytes buffers.
fn split_frames(stream: &[u8]) -> Vec<&[u8]> {
    let mut pos = 0_usize;
    let mut out: Vec<&[u8]> = Vec::new();
    while pos + 2 <= stream.len() {
        let n = (stream[pos] as usize) | ((stream[pos + 1] as usize) << 8);
        let end = pos + n;
        if end > stream.len() || n < 2 {
            break;
        }
        out.push(&stream[pos..end]);
        pos = end;
    }
    out
}

/// Decode one ciphered frame using `cipher` (which advances it once).
fn uncipher(cipher: &mut Cipher, frame: &[u8]) -> Vec<u8> {
    let mut d = frame.to_vec();
    if let Err(e) = cipher.decode(frame, &mut d) {
        eprintln!(
            "[pcapview] cipher error decoding frame of {} bytes: {e}",
            frame.len()
        );
    }
    d
}

fn parse_uscmd(buf: &[u8]) -> u16 {
    (buf[3] as u16) | ((buf[4] as u16) << 8)
}

fn parse_usize(buf: &[u8]) -> usize {
    (buf[0] as usize) | ((buf[1] as usize) << 8)
}

fn print_frame(kind: &str, idx: usize, buf: &[u8]) {
    let total_len = parse_usize(buf);
    let uscmd = parse_uscmd(buf);
    let compressed = uscmd & COMPRESSION_FLAG != 0;
    let real_cmd = uscmd & !COMPRESSION_FLAG;
    let total = total_len;
    let body = buf.get(5..total).unwrap_or(&[]);
    let mut payload = body.to_vec();
    let mut status = String::new();
    if compressed {
        let mut out = vec![0u8; 1 << 20];
        match lzf::decompress(body, &mut out) {
            Ok(n) => {
                payload.clear();
                payload.extend_from_slice(&out[..n]);
                status.push_str(" (LZF)");
            }
            Err(e) => {
                status = format!(" (LZF error: {:?})", e);
            }
        }
    }
    let name = if kind == "game" {
        opcodes_game::from_id(real_cmd)
            .map(|m| format!("{m:?}"))
            .unwrap_or_else(|| "?".to_string())
    } else {
        front_name(real_cmd).to_string()
    };
    println!(
        "  {} #{:<3} usCmd={:<5} ({:<28}) frame={:<5} payload={:<5}{}",
        kind.to_uppercase(),
        idx,
        real_cmd,
        name,
        total,
        payload.len(),
        status
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: pcapview <front|game> <stem>");
        std::process::exit(2);
    }
    let kind = args[1].as_str();
    let stem = args[2].as_str();
    let target = Path::new(stem);
    let c2s = fs::read(target.with_extension("c2s.bin")).expect("c2s.bin missing");
    let s2c = fs::read(target.with_extension("s2c.bin")).expect("s2c.bin missing");

    match kind {
        "front" => analyze_front(&c2s, &s2c),
        "game" => analyze_game(&c2s, &s2c),
        other => {
            eprintln!("unknown kind: {other}");
            std::process::exit(2);
        }
    }
}

fn analyze_front(c2s: &[u8], s2c: &[u8]) {
    // s2c frames are plaintext on the front link; c2s frames are XOR-ciphered with
    // the session key the server sends in its first frame (opcode 30000).
    let mut key1 = 0u8;
    let mut key2 = 0u8;
    for f in split_frames(s2c) {
        let uscmd = parse_uscmd(f);
        let total_len = parse_usize(f);
        let payload = &f[5..total_len];
        if uscmd == 30000 {
            // sResponse_secretkey: bytes [2] and [4] are the session key seeds.
            key1 = payload[2];
            key2 = payload[4];
        }
    }
    println!(
        "front: session key bytes (k1,k2)=({:#04x},{:#04x})",
        key1, key2
    );

    let mut cipher = Cipher::session_cipher(key1, key2);
    println!("front S2C:");
    for (i, f) in split_frames(s2c).iter().enumerate() {
        print_frame("S2C", i, f);
    }

    println!("front C2S (deciphered):");
    for (i, f) in split_frames(c2s).iter().enumerate() {
        let d = uncipher(&mut cipher, f);
        println!("  raw frame {} bytes:", d.len());
        print_frame("c2s", i, &d);
    }
}

fn analyze_game(c2s: &[u8], s2c: &[u8]) {
    // Extract the session key from the server's first S2C frame
    // (opcode 1 = GAMEMSG_RESPONSE_CLIENTCHARUNIQUE, fixed link cipher).
    let link = Cipher::link_cipher();
    let mut session_key1 = 0u8;
    let mut session_key2 = 0u8;
    {
        let mut l = link;
        for f in split_frames(s2c) {
            let d = uncipher(&mut l, f);
            let uscmd = parse_uscmd(&d);
            if uscmd == 1 {
                let total_len = parse_usize(&d);
                let body = &d[5..total_len];
                // cData: SI32 charunique, UI8 key1, UI8 key2, ...
                session_key1 = body[4];
                session_key2 = body[5];
                break;
            }
        }
    }
    println!(
        "game: c2s session key bytes (k1,k2)=({:#04x},{:#04x})",
        session_key1, session_key2
    );

    println!("game S2C (link cipher):");
    let mut link = Cipher::link_cipher();
    let mut all_seen = Vec::new();
    for f in split_frames(s2c).iter() {
        let d = uncipher(&mut link, f);
        all_seen.push(d.clone());
        print_frame("S2C", all_seen.len() - 1, &d);
    }

    let mut cipher = Cipher::session_cipher(session_key1, session_key2);
    println!("game C2S (deciphered):");
    for (i, f) in split_frames(c2s).iter().enumerate() {
        let d = uncipher(&mut cipher, f);
        print_frame("C2S", i, &d);
    }
}
