# codegen

Generates Rust opcode enums from the legacy C++ headers. Development-time only —
nothing in the shipped services depends on this crate.

```bash
cargo run -p codegen -- game           # write crates/gz-proto/src/opcodes_game.rs
cargo run -p codegen -- game --check   # fail if the committed file is stale
cargo run -p codegen -- --help
```

## Targets

| Target | Legacy header | Enum | Generated file |
|---|---|---|---|
| `game` | `GoonZuWorld\CommonLogic\MsgType.h` | `eGameNetworkMsg` | `crates/gz-proto/src/opcodes_game.rs` |

`front` and `auth` (T-003) are added as extra rows in the `TARGETS` table in
`src/main.rs`; nothing else needs to change.

## How the legacy headers are read

The legacy tree is **read-only** and the headers are **EUC-KR / CP949**. The
parser therefore works on raw bytes and never re-encodes the file:

- In CP949 a double-byte character's lead byte is `0x81..=0xFE` and its trail byte
  is `0x41..=0x5A | 0x61..=0x7A | 0x81..=0xFE`. None of the bytes the parser
  treats as syntax (`/`, `*`, `,`, `=`, `;`, `#`, whitespace) can be a trail
  byte, so stripping comments byte-wise can never split a character.
- Every enumerator name is checked to be pure ASCII before it is emitted, so
  mojibake cannot reach the generated Rust source.
- The working copy mixes CP949 and converted UTF-8 files. Byte-level parsing is
  indifferent to which, which is why the tool does not decode at all.

Set `GOONZU_LEGACY_ROOT` to point at a different checkout of the legacy tree.

## The parser refuses to guess

A silently-wrong opcode table is the worst possible failure mode for this
project, so anything ambiguous is a hard error:

- a `#if` / `#ifdef` inside the enum block (values depend on which branch the
  legacy build compiled),
- a non-numeric or arithmetic rebase such as `= BASE + 1`,
- a value that does not fit the 16-bit `usCmd` field,
- an unparseable enumerator line, an unterminated block comment,
- two legacy names that normalise to the same Rust variant (`GAMEMSG_A_B` and
  `GAMEMSG_A__B` both become `AB`).

## Naming

The `GAMEMSG_` prefix is stripped and the remainder is converted to PascalCase,
so `GAMEMSG_REQUEST_CHAT` becomes `GameMsg::RequestChat`. The original name is
always recoverable: it is the variant's doc comment and the return value of
`GameMsg::legacy_name` / `name(id)`, and it carries the legacy line number.

## Reproducibility

Generated files are committed; `scripts/check.ps1` runs `--check` so a header
change that was not regenerated cannot land unnoticed. `--check` compares line
endings insensitively, because this machine's git has `core.autocrlf=true` and a
fresh checkout would otherwise report false drift.
