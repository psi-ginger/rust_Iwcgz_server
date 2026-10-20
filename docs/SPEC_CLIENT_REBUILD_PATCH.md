# SPEC — Client-Rebuild Patch Inventory (Horizon Partition, Approach 1)

**Target binary:** `New Era.exe` — 9,358,336 bytes, SHA-256
`b917ebe868a8b4fd70098c7e78947bcd912b35cbf9cbb61af35dc06aa1f9461a`.
**Method:** Capstone 5.0.7 (Python 3.12) instruction sweep of every executable
section; immediates confirmed by decode (a site counts only when a `cmp` ends
exactly on the immediate), pairs clustered within 48 bytes. Survey run
2026-10-20 as the P2-prep batch of `DESIGN_THREE_TIER_ID_ARCHITECTURE.md` v4 (§8.7,
Approach 1). The generator script is a survey artifact and is not committed.

**Scope.** This is the work-list for Cat's fork build. Every row is a same-width
immediate swap — no instruction grows, no relocation, no code movement. A patcher
replaces the trailing immediate bytes in place. **Per-site role verification is
required before applying** (each disasm is printed for that review); the counts
below reconcile with Gate 0 (§2.3 of the design doc) in §10.

## 1. How to use this spec

1. Work on a copy; keep the original beside the build (sha256 above is the only
   binary this inventory is valid for — any other build must be re-surveyed).
2. Apply Family A, then B, then C — §7 (instance ceiling) is resolved, so every
   non-`TBD` row applies.
3. `new bytes` are the exact replacement for `old bytes`; rows marked `TBD` need
   their branch shape read first — do not guess them.
4. Post-patch verification: re-run this survey against the patched binary and
   diff row-by-row against this inventory — every non-`TBD` row must show its
   `new bytes`; §8 (item guards) and every `TBD` row must be byte-identical to
   `old bytes`.
5. Ship atomically with the server constants flip (`PLAYER_BAND_TOP`, NPC base,
   pool floors — design doc §3.4/§6-P2): a patched client against the interim
   server calls every NPC id `8,500..=19,999` a PC, and a stock client against
   the horizon server calls every PC above `2,999` an NPC. Both halves ship in
   one release.

## 2. Target state (patched client)

| Class | Range | Bound immediates after patch |
|---|---|---|
| PC (IsPC) | `1..=20_000` | not-PC at ≥ `20_001` (`0x4E21`) |
| NPC class (IsNPC, static/town + monsters) | `20_001..=59_999` | low `0x4E21`, high ceiling `0xEA60` (60,000 — covers Tier 3 to 59,999 in this one rebuild, clearing the separate P3 table rebuild, design doc §8 item 6) |
| Instance NPC | `20_001..=29_999` (§7, approved) | ceiling `0x7530` (30,000); strict form `0x752F` (29,999) |
| char tables | 60,000 entries (recommended) | guard `0x4E20 → 0xEA60`; minimal horizon-only alternative `0x7530` (30,000) |

Table-size note: each grown table costs `entries x entry-size` of client heap —
60,000 entries doubles the legacy 30k-class footprint per table; at typical entry
sizes (< 64 B) this is a few MB across all `CharUniqueInfo`-style tables. Confirm
the fork build's allocator handles it before choosing 0xEA60 over 0x7530.

## 3. Direction rule

Every replacement keeps the original instruction and swaps only its immediate.
The compiler emitted both signed (`jge`/`jl`/`jle`/`jg`) and unsigned
(`jae`/`jb`/`jbe`/`ja`) branches; over id ranges they read the same, so each
family sorts its rows into two direction classes:

- **non-strict** (`jae`/`jge` = `>= X`, `jl`/`jb` = `< X`): the immediate is the
  bound itself — `3,000 -> 20,001`, `8,500 -> 20,001`, `20,000 -> 60,000`.
- **strict** (`ja`/`jg` = `> X`, `jle`/`jbe` = `<= X`): the bound sits one out —
  `3,000 -> 20,000`, `8,500 -> 20,000`. The stock off-by-one shape is preserved
  exactly; the ceiling family (§5) swaps the number as-is in both classes.

Rows whose cmp is not followed by a decodable branch are `TBD` — read the
branch shape before patching them.

## 4. Family A — PC / instance floor (`0xBB8` -> 20,001; 370 sites)

Every `cmp ?, 3000` on the char-id axis. Both stock roles share one divide —
the IsPC upper bound (`1..=2999`) and the instance-NPC lower bound — and the
horizon moves that divide to `20_001`: PC `1..=20_000`, everything else starts
at `20_001`, so the patch value does not depend on the row's role. Verify each
row actually reads a char id (an unrelated 3,000 must not be patched); Gate 0b
counted the zero-check-adjacent IsPC subset as 217 of these.

| RVA | file off | old bytes | disasm | jcc | new bytes | rule |
|---|---|---|---|---|---|---|
| `00035209` | `00034609` | `81 7f 04 b8 0b 00 00` | `cmp dword ptr [edi + 4], 0xbb8` | `jge 0x35322` | `81 7f 04 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0003548B` | `0003488B` | `81 fa b8 0b 00 00` | `cmp edx, 0xbb8` | `jl 0x35393` | `81 fa 21 4e 00 00` | floor 3,000 -> 20,001 |
| `000365A6` | `000359A6` | `81 fb b8 0b 00 00` | `cmp ebx, 0xbb8` | `jl 0x36440` | `81 fb 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00036702` | `00035B02` | `81 fa b8 0b 00 00` | `cmp edx, 0xbb8` | `jl 0x36620` | `81 fa 21 4e 00 00` | floor 3,000 -> 20,001 |
| `000C0EF5` | `000C02F5` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0xc0f40` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `000E7D0D` | `000E710D` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0xe7dab` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `001058B7` | `00104CB7` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x1058c6` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0010CF1C` | `0010C31C` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jle 0x10c883` | `3d 20 4e 00 00` | strict form: floor maps to 20,000 |
| `0010CF8D` | `0010C38D` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jle 0x10c883` | `3d 20 4e 00 00` | strict form: floor maps to 20,000 |
| `0010CFFE` | `0010C3FE` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jle 0x10c883` | `3d 20 4e 00 00` | strict form: floor maps to 20,000 |
| `0010D06F` | `0010C46F` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jle 0x10c883` | `3d 20 4e 00 00` | strict form: floor maps to 20,000 |
| `0010D0E0` | `0010C4E0` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jle 0x10c883` | `3d 20 4e 00 00` | strict form: floor maps to 20,000 |
| `001150A1` | `001144A1` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jge 0x1150b0` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0011713C` | `0011653C` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x117193` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0011CECB` | `0011C2CB` | `81 bd c4 fe ff ff b8 0b 00 00` | `cmp dword ptr [ebp - 0x13c], 0xbb8` | `mov` | `TBD` | no bound jcc - read the branch shape |
| `00125C98` | `00125098` | `81 ff b8 0b 00 00` | `cmp edi, 0xbb8` | `jge 0x125caf` | `81 ff 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00125DDC` | `001251DC` | `81 ff b8 0b 00 00` | `cmp edi, 0xbb8` | `jge 0x125df3` | `81 ff 21 4e 00 00` | floor 3,000 -> 20,001 |
| `001269B8` | `00125DB8` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jge 0x1269cf` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00126AFC` | `00125EFC` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jge 0x126b13` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00135FC1` | `001353C1` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x13606a` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00136891` | `00135C91` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x13693a` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `002335A1` | `002329A1` | `81 ff b8 0b 00 00` | `cmp edi, 0xbb8` | `jge 0x2335b8` | `81 ff 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00233C3F` | `0023303F` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x233c5a` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0026CE2B` | `0026C22B` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jle 0x26ce3f` | `3d 20 4e 00 00` | strict form: floor maps to 20,000 |
| `002B70D5` | `002B64D5` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jle 0x2b7183` | `3d 20 4e 00 00` | strict form: floor maps to 20,000 |
| `002B7BE5` | `002B6FE5` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jle 0x2b7c93` | `3d 20 4e 00 00` | strict form: floor maps to 20,000 |
| `002CA79C` | `002C9B9C` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jle 0x2ca7b0` | `3d 20 4e 00 00` | strict form: floor maps to 20,000 |
| `002CB24B` | `002CA64B` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jle 0x2cb36e` | `3d 20 4e 00 00` | strict form: floor maps to 20,000 |
| `002CE963` | `002CDD63` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `ja 0x2cea05` | `3d 20 4e 00 00` | strict form: floor maps to 20,000 |
| `002D4D2A` | `002D412A` | `65 3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x2d4d39` | `65 3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `002D4F2D` | `002D432D` | `65 3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x2d4f3c` | `65 3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `002D5103` | `002D4503` | `65 3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x2d5112` | `65 3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `002D527D` | `002D467D` | `65 3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x2d528c` | `65 3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `002D542A` | `002D482A` | `65 3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x2d5439` | `65 3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0032DEA5` | `0032D2A5` | `81 fb b8 0b 00 00` | `cmp ebx, 0xbb8` | `jge 0x32deb9` | `81 fb 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003389BD` | `00337DBD` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x338a11` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0033AC07` | `0033A007` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x33ac16` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0033E35B` | `0033D75B` | `81 fb b8 0b 00 00` | `cmp ebx, 0xbb8` | `jl 0x33e2f0` | `81 fb 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0033E91A` | `0033DD1A` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x33e8f0` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0033F130` | `0033E530` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x33f0b8` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0035D95C` | `0035CD5C` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x35d920` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0035DDA4` | `0035D1A4` | `81 ff b8 0b 00 00` | `cmp edi, 0xbb8` | `jl 0x35dbb0` | `81 ff 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0035E0BC` | `0035D4BC` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x35dff0` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0036DFC1` | `0036D3C1` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x36e16f` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00372CCD` | `003720CD` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x372cd8` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003750C5` | `003744C5` | `65 81 fb b8 0b 00 00` | `cmp ebx, 0xbb8` | `jge 0x37511f` | `65 81 fb 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003756A5` | `00374AA5` | `65 81 fb b8 0b 00 00` | `cmp ebx, 0xbb8` | `jge 0x3756ff` | `65 81 fb 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00378C09` | `00378009` | `67 81 ff b8 0b 00 00` | `cmp edi, 0xbb8` | `jge 0x378c65` | `67 81 ff 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0037AF15` | `0037A315` | `65 81 fb b8 0b 00 00` | `cmp ebx, 0xbb8` | `jge 0x37af6f` | `65 81 fb 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0037B9C6` | `0037ADC6` | `67 81 ff b8 0b 00 00` | `cmp edi, 0xbb8` | `jge 0x37ba22` | `67 81 ff 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0037C9D5` | `0037BDD5` | `65 81 fb b8 0b 00 00` | `cmp ebx, 0xbb8` | `jge 0x37ca2f` | `65 81 fb 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003878EF` | `00386CEF` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x387911` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003879FF` | `00386DFF` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x387a21` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00387B0F` | `00386F0F` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x387b31` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00387C1F` | `0038701F` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x387c41` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00387D2F` | `0038712F` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x387d51` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00387E3F` | `0038723F` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x387e61` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00387F4F` | `0038734F` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x387f71` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0038842D` | `0038782D` | `81 f9 b8 0b 00 00` | `cmp ecx, 0xbb8` | `jl 0x388442` | `81 f9 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003889AD` | `00387DAD` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3889cf` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0038B1D5` | `0038A5D5` | `65 81 fb b8 0b 00 00` | `cmp ebx, 0xbb8` | `jge 0x38b22f` | `65 81 fb 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0038BE45` | `0038B245` | `65 81 fb b8 0b 00 00` | `cmp ebx, 0xbb8` | `jge 0x38be9f` | `65 81 fb 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0038C9C9` | `0038BDC9` | `67 81 ff b8 0b 00 00` | `cmp edi, 0xbb8` | `jge 0x38ca25` | `67 81 ff 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0038D876` | `0038CC76` | `67 81 ff b8 0b 00 00` | `cmp edi, 0xbb8` | `jge 0x38d8d2` | `67 81 ff 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0039AAC2` | `00399EC2` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jae 0x39aae0` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003A1EA1` | `003A12A1` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jae 0x3a1eb0` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003A28B1` | `003A1CB1` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jae 0x3a28c0` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003A7F57` | `003A7357` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jle 0x3a8030` | `3d 20 4e 00 00` | strict form: floor maps to 20,000 |
| `003A933A` | `003A873A` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3a939b` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003AD8E1` | `003ACCE1` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3adabf` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003ADB40` | `003ACF40` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x3ad860` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003ADB7F` | `003ACF7F` | `81 f9 b8 0b 00 00` | `cmp ecx, 0xbb8` | `jge 0x3adb1f` | `81 f9 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003ADE7B` | `003AD27B` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3adfe3` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003AE213` | `003AD613` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x3ae1f0` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003AE344` | `003AD744` | `81 f9 b8 0b 00 00` | `cmp ecx, 0xbb8` | `jl 0x3ae271` | `81 f9 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003AE3D5` | `003AD7D5` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jge 0x3ae3e2` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003AE548` | `003AD948` | `81 fa b8 0b 00 00` | `cmp edx, 0xbb8` | `jl 0x3ae540` | `81 fa 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003C8AB6` | `003C7EB6` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3c8ff4` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003CD38E` | `003CC78E` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3cd497` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003CD4AD` | `003CC8AD` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3cd619` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003CD62B` | `003CCA2B` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3cd67d` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003CD68F` | `003CCA8F` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3cd6da` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003CD705` | `003CCB05` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3cd85b` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003CD8FE` | `003CCCFE` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3cda0a` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003CDB5C` | `003CCF5C` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3cdc58` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003CE32E` | `003CD72E` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3ce33b` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003CE9A2` | `003CDDA2` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3ce9af` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D0207` | `003CF607` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3d027f` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D08A0` | `003CFCA0` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x3d08e2` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D194D` | `003D0D4D` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x3d19c3` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D5045` | `003D4445` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3d506a` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D51C0` | `003D45C0` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x3d51ee` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D540D` | `003D480D` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3d5477` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D5824` | `003D4C24` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x3d5835` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D584A` | `003D4C4A` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3d5877` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D65A9` | `003D59A9` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3d65c1` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D6BFE` | `003D5FFE` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3d6dd4` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D6D38` | `003D6138` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x3d6d49` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D6D7F` | `003D617F` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3d6d8d` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D6EF2` | `003D62F2` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x3d6f07` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D7286` | `003D6686` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x3d72a5` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D72D8` | `003D66D8` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3d7301` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D751C` | `003D691C` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3d77dd` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D764B` | `003D6A4B` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3d7674` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D77A9` | `003D6BA9` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3d738b` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D7977` | `003D6D77` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x3d7996` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D79CB` | `003D6DCB` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3d79f4` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D7C5A` | `003D705A` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x3d7c78` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D7CAA` | `003D70AA` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3d7ccc` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D8166` | `003D7566` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3d81c3` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D92D5` | `003D86D5` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3d9335` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003D95F7` | `003D89F7` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3d960c` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003DA321` | `003D9721` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x3da00c` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003DB329` | `003DA729` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3db36e` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003DB384` | `003DA784` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3db46e` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003EA171` | `003E9571` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3ea186` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003EA20A` | `003E960A` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3ea215` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003EA54D` | `003E994D` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3ea558` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003EA57C` | `003E997C` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3ea5bd` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003EAAF4` | `003E9EF4` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x3eab13` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003EAB36` | `003E9F36` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3eab5f` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003EAED9` | `003EA2D9` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3eaf42` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003EB23B` | `003EA63B` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3eb250` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003EB2DA` | `003EA6DA` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3eb2e5` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003ECDAD` | `003EC1AD` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3eca2b` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003ECEEF` | `003EC2EF` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x3ecf1d` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003F092F` | `003EFD2F` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x3f0a21` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003F1086` | `003F0486` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x3f10c5` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003F2E12` | `003F2212` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3f2e1d` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003F4B80` | `003F3F80` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3f4de7` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003F6A31` | `003F5E31` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x3f6a45` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003F6D5D` | `003F615D` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x3f6d71` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003F6F8B` | `003F638B` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x3f7088` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003F7293` | `003F6693` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x3f72a7` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `003FA13A` | `003F953A` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x3fa153` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004073B0` | `004067B0` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x40740d` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0040743F` | `0040683F` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x407490` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00407769` | `00406B69` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4077c6` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004077EF` | `00406BEF` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x40783b` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0040F26D` | `0040E66D` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x40f288` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0041183D` | `00410C3D` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x411942` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00411B71` | `00410F71` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x411bec` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00411CBC` | `004110BC` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x411d24` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004120A2` | `004114A2` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x41211c` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0041229E` | `0041169E` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x412307` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00412323` | `00411723` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x41235d` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0041243D` | `0041183D` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4124b8` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00412566` | `00411966` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4125ce` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00412663` | `00411A63` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4126cb` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00412D40` | `00412140` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x412d50` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00412DF7` | `004121F7` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x412e0d` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0041317E` | `0041257E` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x413194` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004134C2` | `004128C2` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4134d8` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00414023` | `00413423` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x4140b6` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0041409D` | `0041349D` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jle 0x4140d6` | `3d 20 4e 00 00` | strict form: floor maps to 20,000 |
| `004140C8` | `004134C8` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4140d6` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00415512` | `00414912` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x41554d` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0041901A` | `0041841A` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x41903b` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0041B7D5` | `0041ABD5` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x41b864` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0041C4BD` | `0041B8BD` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x41c563` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0041CCCF` | `0041C0CF` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x41cce4` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0041DDFB` | `0041D1FB` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x41df0f` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0041E934` | `0041DD34` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x41e956` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0041E9F6` | `0041DDF6` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x41ea4f` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0041EC1E` | `0041E01E` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x41ece8` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0041F06E` | `0041E46E` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x41f13b` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0041F14D` | `0041E54D` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x41f15d` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0041F1D1` | `0041E5D1` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x41ffdf` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0042005B` | `0041F45B` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x420074` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00420DDC` | `004201DC` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x420df1` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0042258B` | `0042198B` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x422727` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00424CAE` | `004240AE` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x424d0b` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00424F73` | `00424373` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x424f88` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0042751D` | `0042691D` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4277d8` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004279D4` | `00426DD4` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x427a35` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00428686` | `00427A86` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4286ed` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00428D55` | `00428155` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x428dba` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00429EFE` | `004292FE` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x429fe3` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0042A3CC` | `004297CC` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x42a4f1` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0042A40C` | `0042980C` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x42a4f1` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0042A6ED` | `00429AED` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x42a979` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0042AB28` | `00429F28` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x42ab89` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0042FC41` | `0042F041` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x42fc5a` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004437EB` | `00442BEB` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44387e` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004438CE` | `00442CCE` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44395a` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00443992` | `00442D92` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4439ac` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00443A14` | `00442E14` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x443cb8` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00443A4B` | `00442E4B` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44395a` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004446BF` | `00443ABF` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4446cd` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004448BB` | `00443CBB` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x4448fb` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00444B04` | `00443F04` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x444b2c` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00444C5F` | `0044405F` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x444c79` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00444F06` | `00444306` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x444f5d` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044593B` | `00444D3B` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x445972` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00445988` | `00444D88` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x445f48` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00445F5E` | `0044535E` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x445fe7` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00446279` | `00445679` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4463b4` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004463C6` | `004457C6` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4463f4` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004467AA` | `00445BAA` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4467c4` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004470F5` | `004464F5` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x447116` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004475CB` | `004469CB` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x447beb` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00447E03` | `00447203` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x447e83` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00447EC2` | `004472C2` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4480dc` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004480F2` | `004474F2` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44844c` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00448462` | `00447862` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44880f` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00448851` | `00447C51` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4489de` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044B3FE` | `0044A7FE` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44b6d0` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044BBE0` | `0044AFE0` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44bc73` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044C0FF` | `0044B4FF` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44c24e` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044C562` | `0044B962` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44c686` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044C619` | `0044BA19` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44c685` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044C75D` | `0044BB5D` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44c85b` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044C86D` | `0044BC6D` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44c88e` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044C8D1` | `0044BCD1` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44c915` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044CEBA` | `0044C2BA` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44cf25` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044D0A8` | `0044C4A8` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44d11d` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044D20E` | `0044C60E` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44d255` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044D2C8` | `0044C6C8` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44d2ee` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044D304` | `0044C704` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44daab` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044DAF3` | `0044CEF3` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44dc4e` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044DC9C` | `0044D09C` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x44dcfc` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044DCDB` | `0044D0DB` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44de9b` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044E07A` | `0044D47A` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x44e0a9` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044E223` | `0044D623` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44e24f` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044E34A` | `0044D74A` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44e391` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044E414` | `0044D814` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44e4f6` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044E548` | `0044D948` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44e62b` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044E643` | `0044DA43` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44e682` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044E6C2` | `0044DAC2` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44e78f` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044E7D2` | `0044DBD2` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x44e839` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044E9E6` | `0044DDE6` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44ea97` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044EAFD` | `0044DEFD` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x44eb17` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044ED26` | `0044E126` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44ed42` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044EE40` | `0044E240` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x44ee5a` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044F545` | `0044E945` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44f5b6` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044F606` | `0044EA06` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44f64d` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044FB2A` | `0044EF2A` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x44fb4e` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0044FB64` | `0044EF64` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4502fc` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00450332` | `0044F732` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x450495` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004504D4` | `0044F8D4` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x450619` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0045062F` | `0044FA2F` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4506a2` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004506CF` | `0044FACF` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4506f9` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00450ABC` | `0044FEBC` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x450aeb` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00451003` | `00450403` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x451047` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004510DA` | `004504DA` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4511c4` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004511DE` | `004505DE` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x45121f` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0045140D` | `0045080D` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x45149f` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00451476` | `00450876` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x4515dd` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00451825` | `00450C25` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x45184a` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004519C5` | `00450DC5` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x451a0f` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00451B50` | `00450F50` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x452149` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00451D97` | `00451197` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x452149` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0045241B` | `0045181B` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4525c6` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00452BE4` | `00451FE4` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x452bfe` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00452D2C` | `0045212C` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x452eaa` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00452E1B` | `0045221B` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x452eaa` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00452F4E` | `0045234E` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x45301c` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004530AC` | `004524AC` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4530cb` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004530E1` | `004524E1` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x453431` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00453447` | `00452847` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4538f3` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0045386B` | `00452C6B` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x4538f3` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00453921` | `00452D21` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x453a32` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0046568F` | `00464A8F` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jle 0x465739` | `3d 20 4e 00 00` | strict form: floor maps to 20,000 |
| `004685C7` | `004679C7` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x468501` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004687A3` | `00467BA3` | `81 fb b8 0b 00 00` | `cmp ebx, 0xbb8` | `jl 0x468670` | `81 fb 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00468C2B` | `0046802B` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x468ba0` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004A333C` | `004A273C` | `81 be 08 41 00 00 b8 0b 00 00` | `cmp dword ptr [esi + 0x4108], 0xbb8` | `jle 0x4a3440` | `81 be 08 41 00 00 20 4e 00 00` | strict form: floor maps to 20,000 |
| `004D40A7` | `004D34A7` | `81 f9 b8 0b 00 00` | `cmp ecx, 0xbb8` | `jl 0x4d410e` | `81 f9 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004D4528` | `004D3928` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x4d4577` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004D4BF9` | `004D3FF9` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x4d4c42` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004D659E` | `004D599E` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jge 0x4d65ae` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004D662B` | `004D5A2B` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jge 0x4d663b` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `004D6708` | `004D5B08` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x4d66a0` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00504582` | `00503982` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jbe 0x50461c` | `3d 20 4e 00 00` | strict form: floor maps to 20,000 |
| `005146A4` | `00513AA4` | `81 f9 b8 0b 00 00` | `cmp ecx, 0xbb8` | `jle 0x5146b7` | `81 f9 20 4e 00 00` | strict form: floor maps to 20,000 |
| `00541053` | `00540453` | `81 78 08 b8 0b 00 00` | `cmp dword ptr [eax + 8], 0xbb8` | `mov` | `TBD` | no bound jcc - read the branch shape |
| `005625B0` | `005619B0` | `81 ff b8 0b 00 00` | `cmp edi, 0xbb8` | `jl 0x562580` | `81 ff 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00562C6F` | `0056206F` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x562c42` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0056A85D` | `00569C5D` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x56a8a0` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0056A9DD` | `00569DDD` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x56aa11` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0056D3FE` | `0056C7FE` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jle 0x56d40f` | `3d 20 4e 00 00` | strict form: floor maps to 20,000 |
| `005742CD` | `005736CD` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x57430a` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00574416` | `00573816` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x57444d` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `005744FF` | `005738FF` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x574529` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `005745FB` | `005739FB` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x574626` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00574AA8` | `00573EA8` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x574add` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `005751C4` | `005745C4` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x575296` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0057621A` | `0057561A` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x57629b` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `005768AC` | `00575CAC` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x576ddb` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00577766` | `00576B66` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x57777d` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00577B3E` | `00576F3E` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x577c5b` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00578638` | `00577A38` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x57864c` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `005796AD` | `00578AAD` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x5796d2` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0057997D` | `00578D7D` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x5799b1` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00579B12` | `00578F12` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x579b46` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00579C6F` | `0057906F` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x579ca3` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00579D98` | `00579198` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x579dcc` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0057F11A` | `0057E51A` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x57f78c` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `005800F2` | `0057F4F2` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x580104` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00580386` | `0057F786` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x5805bc` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0058290A` | `00581D0A` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jge 0x5829f8` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00585024` | `00584424` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x586510` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0058505F` | `0058445F` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x586510` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `005863A3` | `005857A3` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x58648e` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `005866DA` | `00585ADA` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x58670f` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `005869A5` | `00585DA5` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x586a5b` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00586A83` | `00585E83` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x586a98` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00586BEB` | `00585FEB` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x586c30` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00586CDF` | `005860DF` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x586db0` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00586D35` | `00586135` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x586db0` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00586F3F` | `0058633F` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x587056` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00586F76` | `00586376` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x587056` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `005870DA` | `005864DA` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x587293` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00587111` | `00586511` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x587293` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0058765F` | `00586A5F` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x5876a2` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0058770E` | `00586B0E` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x5890f9` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00588F87` | `00588387` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x589077` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `005918E1` | `00590CE1` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x5918f2` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00591976` | `00590D76` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x591b13` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00591A09` | `00590E09` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x591a1a` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00591AFB` | `00590EFB` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x591b0c` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `005BF2EA` | `005BE6EA` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x5bf2b0` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `005C1C2A` | `005C102A` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x5c1bf0` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `005C734D` | `005C674D` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x5c72b0` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `005CF5F8` | `005CE9F8` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x5cf594` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `005D5ECE` | `005D52CE` | `81 bb 74 09 00 00 b8 0b 00 00` | `cmp dword ptr [ebx + 0x974], 0xbb8` | `ja 0x5d61c1` | `81 bb 74 09 00 00 20 4e 00 00` | strict form: floor maps to 20,000 |
| `005D648D` | `005D588D` | `81 bf 74 09 00 00 b8 0b 00 00` | `cmp dword ptr [edi + 0x974], 0xbb8` | `ja 0x5d6b6b` | `81 bf 74 09 00 00 20 4e 00 00` | strict form: floor maps to 20,000 |
| `005E1200` | `005E0600` | `81 bf 74 09 00 00 b8 0b 00 00` | `cmp dword ptr [edi + 0x974], 0xbb8` | `ja 0x5e13c8` | `81 bf 74 09 00 00 20 4e 00 00` | strict form: floor maps to 20,000 |
| `005E6018` | `005E5418` | `81 bb 74 09 00 00 b8 0b 00 00` | `cmp dword ptr [ebx + 0x974], 0xbb8` | `jbe 0x5e6084` | `81 bb 74 09 00 00 20 4e 00 00` | strict form: floor maps to 20,000 |
| `005E6624` | `005E5A24` | `81 be 74 09 00 00 b8 0b 00 00` | `cmp dword ptr [esi + 0x974], 0xbb8` | `ja 0x5e63ff` | `81 be 74 09 00 00 20 4e 00 00` | strict form: floor maps to 20,000 |
| `005E7004` | `005E6404` | `81 bf 74 09 00 00 b8 0b 00 00` | `cmp dword ptr [edi + 0x974], 0xbb8` | `ja 0x5e747e` | `81 bf 74 09 00 00 20 4e 00 00` | strict form: floor maps to 20,000 |
| `005E8EE1` | `005E82E1` | `81 be 74 09 00 00 b8 0b 00 00` | `cmp dword ptr [esi + 0x974], 0xbb8` | `jbe 0x5e8f3d` | `81 be 74 09 00 00 20 4e 00 00` | strict form: floor maps to 20,000 |
| `005E9171` | `005E8571` | `81 bf 74 09 00 00 b8 0b 00 00` | `cmp dword ptr [edi + 0x974], 0xbb8` | `jbe 0x5e91ac` | `81 bf 74 09 00 00 20 4e 00 00` | strict form: floor maps to 20,000 |
| `005E9505` | `005E8905` | `81 be 74 09 00 00 b8 0b 00 00` | `cmp dword ptr [esi + 0x974], 0xbb8` | `ja 0x5e96ae` | `81 be 74 09 00 00 20 4e 00 00` | strict form: floor maps to 20,000 |
| `005E9722` | `005E8B22` | `81 bf 74 09 00 00 b8 0b 00 00` | `cmp dword ptr [edi + 0x974], 0xbb8` | `ja 0x5e9738` | `81 bf 74 09 00 00 20 4e 00 00` | strict form: floor maps to 20,000 |
| `005E97B0` | `005E8BB0` | `81 bf 74 09 00 00 b8 0b 00 00` | `cmp dword ptr [edi + 0x974], 0xbb8` | `jbe 0x5e97f1` | `81 bf 74 09 00 00 20 4e 00 00` | strict form: floor maps to 20,000 |
| `005E9B30` | `005E8F30` | `81 be 74 09 00 00 b8 0b 00 00` | `cmp dword ptr [esi + 0x974], 0xbb8` | `jbe 0x5e9b6e` | `81 be 74 09 00 00 20 4e 00 00` | strict form: floor maps to 20,000 |
| `005EA309` | `005E9709` | `81 bf 74 09 00 00 b8 0b 00 00` | `cmp dword ptr [edi + 0x974], 0xbb8` | `jbe 0x5ea36f` | `81 bf 74 09 00 00 20 4e 00 00` | strict form: floor maps to 20,000 |
| `005EA7D9` | `005E9BD9` | `81 be 74 09 00 00 b8 0b 00 00` | `cmp dword ptr [esi + 0x974], 0xbb8` | `ja 0x5ea856` | `81 be 74 09 00 00 20 4e 00 00` | strict form: floor maps to 20,000 |
| `005EA8CA` | `005E9CCA` | `81 be 74 09 00 00 b8 0b 00 00` | `cmp dword ptr [esi + 0x974], 0xbb8` | `ja 0x5ea928` | `81 be 74 09 00 00 20 4e 00 00` | strict form: floor maps to 20,000 |
| `005EA9AF` | `005E9DAF` | `81 bb 74 09 00 00 b8 0b 00 00` | `cmp dword ptr [ebx + 0x974], 0xbb8` | `ja 0x5ead97` | `81 bb 74 09 00 00 20 4e 00 00` | strict form: floor maps to 20,000 |
| `005EAEC9` | `005EA2C9` | `81 bb 74 09 00 00 b8 0b 00 00` | `cmp dword ptr [ebx + 0x974], 0xbb8` | `mov` | `TBD` | no bound jcc - read the branch shape |
| `006036BA` | `00602ABA` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x603748` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00603817` | `00602C17` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x603894` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `006038FC` | `00602CFC` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x60392a` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `006039BE` | `00602DBE` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x603a27` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00603AEF` | `00602EEF` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x603b43` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00603B76` | `00602F76` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x603bb4` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00603C6B` | `0060306B` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x603ce8` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00603D2E` | `0060312E` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x603eb3` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00603F4B` | `0060334B` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x603fa4` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00603FEE` | `006033EE` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x6040fd` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0060419A` | `0060359A` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x604400` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `006042DD` | `006036DD` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jl 0x6043d7` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0060443D` | `0060383D` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x604494` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0060DA32` | `0060CE32` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jge 0x60da44` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0060DB44` | `0060CF44` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jge 0x60db53` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0060F402` | `0060E802` | `65 3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x60f411` | `65 3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00619ACD` | `00618ECD` | `81 bb 74 09 00 00 b8 0b 00 00` | `cmp dword ptr [ebx + 0x974], 0xbb8` | `ja 0x61a50a` | `81 bb 74 09 00 00 20 4e 00 00` | strict form: floor maps to 20,000 |
| `0061A9FE` | `00619DFE` | `81 ff b8 0b 00 00` | `cmp edi, 0xbb8` | `jl 0x61a830` | `81 ff 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0061B6D4` | `0061AAD4` | `3d b8 0b 00 00` | `cmp eax, 0xbb8` | `jge 0x61b6e4` | `3d 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0061D4BA` | `0061C8BA` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x61d110` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0061DA9B` | `0061CE9B` | `81 f9 b8 0b 00 00` | `cmp ecx, 0xbb8` | `jl 0x61d670` | `81 f9 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0063F10F` | `0063E50F` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jge 0x63f1e4` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00646C12` | `00646012` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x646be2` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `006592FE` | `006586FE` | `81 bd 34 ec ff ff b8 0b 00 00` | `cmp dword ptr [ebp - 0x13cc], 0xbb8` | `jl 0x65935b` | `81 bd 34 ec ff ff 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00659E83` | `00659283` | `81 bd 34 ec ff ff b8 0b 00 00` | `cmp dword ptr [ebp - 0x13cc], 0xbb8` | `jl 0x659ee0` | `81 bd 34 ec ff ff 21 4e 00 00` | floor 3,000 -> 20,001 |
| `00680FE2` | `006803E2` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x680f70` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `0069C728` | `0069BB28` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x69c710` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |
| `006A1F0A` | `006A130A` | `81 fe b8 0b 00 00` | `cmp esi, 0xbb8` | `jl 0x6a1ee0` | `81 fe 21 4e 00 00` | floor 3,000 -> 20,001 |

## 5. Family B — char-table / NPC-ceiling guards (`0x4E20` -> 60,000; 456 sites)

Every `cmp ?, 20000` outside an IsNPC pair (§6). Both stock roles — table-index
guards and the IsNPC upper bound — aim at the same ceiling, so all rows take
`0xEA60` (60,000): tables grown to 60,000 entries clear Tier 3 (to 59,999) in
this one rebuild, design doc §8 item 6. If the fork build chooses the minimal
horizon-only size instead, use `0x7530` (30,000) for every row here. A strict
row that validates the immediate as an index passed 20,000 in stock and passes
60,000 now — identical shape; check that row at review.

| RVA | file off | old bytes | disasm | jcc | new bytes | rule |
|---|---|---|---|---|---|---|
| `0004D5AB` | `0004C9AB` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jge 0x4d5bf` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0004E47E` | `0004D87E` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x4e52e` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00056C42` | `00056042` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x56cf2` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00058575` | `00057975` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jge 0x58589` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `000585C0` | `000579C0` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jae 0x585e3` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `000586CB` | `00057ACB` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jae 0x586fb` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0005873D` | `00057B3D` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jae 0x58769` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `000587B3` | `00057BB3` | `36 81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jae 0x587c8` | `36 81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `000587EA` | `00057BEA` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jae 0x58810` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00058856` | `00057C56` | `36 81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jae 0x5886b` | `36 81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0005888D` | `00057C8D` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jae 0x588fc` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0005AD80` | `0005A180` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x5ad97` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0005C59D` | `0005B99D` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x5c5d0` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0005DFF4` | `0005D3F4` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x5e008` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00063680` | `00062A80` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x636a3` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `000698F8` | `00068CF8` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x69991` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00079595` | `00078995` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jle 0x795a2` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0007C508` | `0007B908` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jle 0x7c598` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00087FC5` | `000873C5` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x87fe3` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0008DF11` | `0008D311` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x8e076` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `000C4809` | `000C3C09` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jg 0xc4914` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `000E3C8F` | `000E308F` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0xe3cb1` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `000E7CF0` | `000E70F0` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0xe7d35` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00138606` | `00137A06` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jl 0x138629` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00145C9C` | `0014509C` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x145cbe` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0015D9F3` | `0015CDF3` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x15da11` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0015F733` | `0015EB33` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x15f751` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `001872C4` | `001866C4` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x1872dd` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `001ED31B` | `001EC71B` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x1ed33d` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `002446BA` | `00243ABA` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x2446d8` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0026771A` | `00266B1A` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x267743` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `002BD8A1` | `002BCCA1` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x2bd8c7` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `002E3B4C` | `002E2F4C` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x2e3b6a` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0033717D` | `0033657D` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jle 0x33722f` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0033C35A` | `0033B75A` | `81 3c 01 20 4e 00 00` | `cmp dword ptr [ecx + eax], 0x4e20` | `jge 0x33c3ae` | `81 3c 01 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0033C867` | `0033BC67` | `81 3c 01 20 4e 00 00` | `cmp dword ptr [ecx + eax], 0x4e20` | `jge 0x33c8bb` | `81 3c 01 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0033EC18` | `0033E018` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x33ec36` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00341020` | `00340420` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x34102e` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00341275` | `00340675` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x341283` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003414ED` | `003408ED` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x34150b` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00341536` | `00340936` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jge 0x34154f` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003419CB` | `00340DCB` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x3419d9` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00342BFA` | `00341FFA` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x342c08` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00342E5B` | `0034225B` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x342e74` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00343993` | `00342D93` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x3439a1` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00343ACB` | `00342ECB` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x343ad9` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00343C69` | `00343069` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x343c77` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003446D5` | `00343AD5` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x3446e3` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00344714` | `00343B14` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x344722` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00344835` | `00343C35` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x34484e` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00344887` | `00343C87` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x3448a0` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003448C2` | `00343CC2` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x3448db` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00344CC4` | `003440C4` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x344cd9` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00344D20` | `00344120` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x344d2e` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00344E58` | `00344258` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x344e96` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00344F34` | `00344334` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x344f57` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00345291` | `00344691` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x34529f` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003452DB` | `003446DB` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x3452ef` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0034564B` | `00344A4B` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x34566b` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003458A7` | `00344CA7` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x3458b5` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0034D66F` | `0034CA6F` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x34d683` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0034D6BD` | `0034CABD` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x34d6d1` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0034DC5C` | `0034D05C` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x34dc6a` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0034DCD3` | `0034D0D3` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x34dce7` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0035865A` | `00357A5A` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x358677` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0035DB48` | `0035CF48` | `3e 81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jge 0x35db5d` | `3e 81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0035E159` | `0035D559` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jl 0x35e0d0` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00363BEE` | `00362FEE` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jl 0x363c15` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00363C1F` | `0036301F` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x363c3e` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0036A809` | `00369C09` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jae 0x36a818` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0036A8B7` | `00369CB7` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x36a8c6` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0036E40B` | `0036D80B` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x36e42b` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0036E475` | `0036D875` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x36e4b1` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0036FC28` | `0036F028` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x36fc36` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00370138` | `0036F538` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x37015c` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003701B2` | `0036F5B2` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x3701d1` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003702B6` | `0036F6B6` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x3702da` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00370330` | `0036F730` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x37034f` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003746A0` | `00373AA0` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x3746b8` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00374C57` | `00374057` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jl 0x374c6f` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00375280` | `00374680` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x375298` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00375860` | `00374C60` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x375878` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003764F7` | `003758F7` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jl 0x37650f` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003769D0` | `00375DD0` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jl 0x3769e8` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00377037` | `00376437` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jl 0x37704f` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00377647` | `00376A47` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jl 0x37765f` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00377C47` | `00377047` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jl 0x377c5f` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00378247` | `00377647` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jl 0x37825f` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00378790` | `00377B90` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jl 0x3787a8` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00379030` | `00378430` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x379048` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003796A0` | `00378AA0` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jl 0x3796b8` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00379E77` | `00379277` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jl 0x379e8f` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0037A4E7` | `003798E7` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jl 0x37a4ff` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0037AAA0` | `00379EA0` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jl 0x37aab8` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0037B0D0` | `0037A4D0` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x37b0e8` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0037B564` | `0037A964` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x37b57c` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0037BBD0` | `0037AFD0` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x37bbe8` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0037C4DF` | `0037B8DF` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jl 0x37c4f7` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0037CB90` | `0037BF90` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x37cba8` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0038B390` | `0038A790` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x38b3a8` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0038B9A0` | `0038ADA0` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jl 0x38b9b8` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0038C000` | `0038B400` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x38c018` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0038C557` | `0038B957` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jl 0x38c56f` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0038CD90` | `0038C190` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jl 0x38cda8` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0038D3F7` | `0038C7F7` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jl 0x38d40f` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0038DB00` | `0038CF00` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jl 0x38db18` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0038E077` | `0038D477` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jl 0x38e08f` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0038E647` | `0038DA47` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jl 0x38e65f` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0038F547` | `0038E947` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jl 0x38f55f` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0038FCD0` | `0038F0D0` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jl 0x38fce8` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0039437B` | `0039377B` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x394399` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0039B3A1` | `0039A7A1` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x39b3af` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0039B8C8` | `0039ACC8` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x39b8e5` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0039B927` | `0039AD27` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x39b940` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0039BA16` | `0039AE16` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x39ba33` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0039BA75` | `0039AE75` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x39ba8e` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003A5B1B` | `003A4F1B` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x3a5b6c` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003A6A95` | `003A5E95` | `f3 3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x3a6a89` | `f3 3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003ADACC` | `003ACECC` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jge 0x3adae0` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003AF3DF` | `003AE7DF` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x3af481` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003B3BA1` | `003B2FA1` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x3b3bbf` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003B3C5B` | `003B305B` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x3b3c79` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003CD895` | `003CCC95` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x3cd8e8` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003D0892` | `003CFC92` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x3d08b5` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003D6659` | `003D5A59` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x3d67c7` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003D7278` | `003D6678` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x3d729b` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003D7969` | `003D6D69` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x3d798c` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003D7C4C` | `003D704C` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x3d7c6f` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003EAAE6` | `003E9EE6` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x3eab09` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003EB900` | `003EAD00` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x3eb912` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003EFC84` | `003EF084` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x3efc72` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003EFEDA` | `003EF2DA` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x3eff43` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003EFF3A` | `003EF33A` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x3efef0` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003F1053` | `003F0453` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x3f10c0` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003F188A` | `003F0C8A` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x3f189c` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003F1AE2` | `003F0EE2` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x3f1bd9` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003F1BC9` | `003F0FC9` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x3f1bd9` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003F1C8B` | `003F108B` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x3f1c9c` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003F1CC1` | `003F10C1` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x3f1cd3` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003F1D10` | `003F1110` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x3f1d1d` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003F1D5A` | `003F115A` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x3f1d6a` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003F1D97` | `003F1197` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x3f1daa` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003F1DD6` | `003F11D6` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x3f1de7` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003F7D7C` | `003F717C` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x3f7d9a` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003F8B1D` | `003F7F1D` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x3f8b3b` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003F8F3B` | `003F833B` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jl 0x3f8ed2` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `003FA128` | `003F9528` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x3fa55f` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00412859` | `00411C59` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x412886` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00412E22` | `00412222` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x412e44` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `004131A9` | `004125A9` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x4131cb` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `004134ED` | `004128ED` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x41353e` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0041364D` | `00412A4D` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x41366c` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00414015` | `00413415` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x41403c` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0041A57D` | `0041997D` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x41a59b` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0041EA68` | `0041DE68` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x41eb5f` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00420049` | `0041F449` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x420101` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0042118B` | `0042058B` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x4211a9` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00426B67` | `00425F67` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x426d31` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0042A3BE` | `004297BE` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x42a3e9` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0042A3FE` | `004297FE` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x42a429` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0042A536` | `00429936` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `ja 0x42a598` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0042FC2F` | `0042F02F` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x42fd77` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0043C497` | `0043B897` | `81 b8 48 01 00 00 20 4e 00 00` | `cmp dword ptr [eax + 0x148], 0x4e20` | `jae 0x43c4b2` | `81 b8 48 01 00 00 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0044240C` | `0044180C` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x442433` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `004623B1` | `004617B1` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x4623cf` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00462EAF` | `004622AF` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x462ecd` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00463118` | `00462518` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x463136` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0046334B` | `0046274B` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x463369` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00463578` | `00462978` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x463596` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `004637AB` | `00462BAB` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x4637c9` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00466B86` | `00465F86` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jge 0x466b9f` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00466D2E` | `0046612E` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jge 0x466d47` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0046829E` | `0046769E` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x4682b7` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `004D4B6B` | `004D3F6B` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jge 0x4d4ba9` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `004D7051` | `004D6451` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x4d706a` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `004DDF9C` | `004DD39C` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jg 0x4de012` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `004DE37D` | `004DD77D` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jg 0x4de3f5` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `004DE847` | `004DDC47` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jg 0x4de8ab` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `004DF511` | `004DE911` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jg 0x4df590` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `004E2661` | `004E1A61` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x4e2675` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `004E46B0` | `004E3AB0` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x4e46a5` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `004E4C4B` | `004E404B` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x4e4c40` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `004E955C` | `004E895C` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x4e957a` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `004EFAD4` | `004EEED4` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x4efaf2` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `004F009B` | `004EF49B` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x4f00b9` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `004F05DC` | `004EF9DC` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x4f06ed` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `004F073F` | `004EFB3F` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x4f0775` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0052DA6C` | `0052CE6C` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x52dac5` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0052DDDC` | `0052D1DC` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x52de35` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0052E18F` | `0052D58F` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x52e1e8` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0056873A` | `00567B3A` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x568753` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0056950B` | `0056890B` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x56951f` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `005695EB` | `005689EB` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x569612` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00569AAD` | `00568EAD` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x569ac6` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0056B20F` | `0056A60F` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x56b236` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0056B2AB` | `0056A6AB` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x56b2c5` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0056B845` | `0056AC45` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x56b859` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0056B97A` | `0056AD7A` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x56b991` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0056CAF8` | `0056BEF8` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x56cb11` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0056D48E` | `0056C88E` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x56d4a7` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0056E287` | `0056D687` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x56e2a6` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0056E349` | `0056D749` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jge 0x56e28f` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00574A8F` | `00573E8F` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x574ac8` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057A2CA` | `005796CA` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x57a2f6` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057A366` | `00579766` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x57a38a` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057A4C2` | `005798C2` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x57a4d9` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057A663` | `00579A63` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x57a67a` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057A7E6` | `00579BE6` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x57a7fd` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057A969` | `00579D69` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x57a980` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057AA54` | `00579E54` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x57aa6b` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057AAB5` | `00579EB5` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x57ab44` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057AC54` | `0057A054` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x57ac6b` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057AD25` | `0057A125` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x57ad3c` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057AD82` | `0057A182` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x57ae11` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057AEFE` | `0057A2FE` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x57af15` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057AF82` | `0057A382` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x57af99` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057AFE3` | `0057A3E3` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x57b072` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057B15F` | `0057A55F` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x57b176` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057B1E3` | `0057A5E3` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x57b1fa` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057B244` | `0057A644` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x57b2d3` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057B3C0` | `0057A7C0` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x57b3d7` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057C1D7` | `0057B5D7` | `81 fa 20 4e 00 00` | `cmp edx, 0x4e20` | `jl 0x57c1fc` | `81 fa 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0057E29A` | `0057D69A` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jl 0x57e2b9` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00580E9A` | `0058029A` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x580ebf` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00582255` | `00581655` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x58226f` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `005822FB` | `005816FB` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x582345` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00582870` | `00581C70` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jge 0x582884` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `005828BB` | `00581CBB` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x5828ce` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0058565D` | `00584A5D` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x58567b` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `005866C1` | `00585AC1` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jl 0x5866fa` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00590A6E` | `0058FE6E` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x590a8c` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00592537` | `00591937` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jl 0x592554` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00594E34` | `00594234` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jl 0x594e53` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `005956C8` | `00594AC8` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jl 0x5956e7` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0059594E` | `00594D4E` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jl 0x595971` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `005BF212` | `005BE612` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x5bf230` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `005D2DD0` | `005D21D0` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jb 0x5d2ded` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `005F1DB1` | `005F11B1` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x5f1dcf` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `005F2D2D` | `005F212D` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jl 0x5f2faf` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `005FE531` | `005FD931` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x5fe500` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00604D5C` | `0060415C` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x604d7a` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00604E11` | `00604211` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x604e2f` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00604EB7` | `006042B7` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x604ef9` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00604F81` | `00604381` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x604f9f` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0060507E` | `0060447E` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x60509c` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00606330` | `00605730` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x60634d` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006074FD` | `006068FD` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x607524` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006103FA` | `0060F7FA` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x61041d` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00610ACE` | `0060FECE` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x610adc` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00610B5A` | `0060FF5A` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x610b70` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00610BFE` | `0060FFFE` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x610c0c` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00610C51` | `00610051` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x610c5f` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00610CA6` | `006100A6` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x610cb4` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00611F4E` | `0061134E` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jl 0x611f6d` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00615597` | `00614997` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x6155a7` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00615D8A` | `0061518A` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x615db1` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00616D03` | `00616103` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x616d21` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0061801A` | `0061741A` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x618041` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00619A19` | `00618E19` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x619a40` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0062335D` | `0062275D` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jb 0x62337a` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00625146` | `00624546` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jb 0x625163` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0062BD7E` | `0062B17E` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jb 0x62bd9b` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0062CDCB` | `0062C1CB` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jb 0x62cde8` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00633C96` | `00633096` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x633cb4` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00636337` | `00635737` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x6363ca` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00636685` | `00635A85` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x6366b1` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006388E0` | `00637CE0` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x6388ff` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006389D8` | `00637DD8` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x6389f7` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0063BB41` | `0063AF41` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x63bb55` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0063BCC1` | `0063B0C1` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x63bce3` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0063BF01` | `0063B301` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x63bf1c` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0063C925` | `0063BD25` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x63c939` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0063D06A` | `0063C46A` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x63d07e` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0063D4DF` | `0063C8DF` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x63d4f3` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0063D8AC` | `0063CCAC` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jae 0x63d8c0` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0063DF74` | `0063D374` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x63e338` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0063E3D1` | `0063D7D1` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x63edc9` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0063EA99` | `0063DE99` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x63eab9` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0063EAE6` | `0063DEE6` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x63eb05` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0063EE2D` | `0063E22D` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x63eefc` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00640594` | `0063F994` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x6405b2` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006405DD` | `0063F9DD` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x6405f7` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006451FE` | `006445FE` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x64521c` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0064563E` | `00644A3E` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jl 0x64565c` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00645C36` | `00645036` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x645c4a` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006467FE` | `00645BFE` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x646812` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00647692` | `00646A92` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jge 0x6476a0` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006477A4` | `00646BA4` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x6477b1` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00648C43` | `00648043` | `81 fa 20 4e 00 00` | `cmp edx, 0x4e20` | `jge 0x648c58` | `81 fa 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00648D70` | `00648170` | `81 bd d4 eb ff ff 20 4e 00 00` | `cmp dword ptr [ebp - 0x142c], 0x4e20` | `jge 0x648d9b` | `81 bd d4 eb ff ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00648E97` | `00648297` | `81 bd d4 eb ff ff 20 4e 00 00` | `cmp dword ptr [ebp - 0x142c], 0x4e20` | `jge 0x648eba` | `81 bd d4 eb ff ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00649886` | `00648C86` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x64989a` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0064F9C8` | `0064EDC8` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x64f9dc` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0064FD49` | `0064F149` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jae 0x64fd63` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006505A3` | `0064F9A3` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x6505b7` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00650E5E` | `0065025E` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x650e72` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0065320C` | `0065260C` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x653226` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006545F6` | `006539F6` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jge 0x65460f` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00654AAA` | `00653EAA` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x654ac4` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00655A93` | `00654E93` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x655aa7` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00655E19` | `00655219` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x655e2d` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006580CC` | `006574CC` | `81 fa 20 4e 00 00` | `cmp edx, 0x4e20` | `jae 0x6580da` | `81 fa 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0065B273` | `0065A673` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x65b287` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0065E177` | `0065D577` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x65e18b` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0065E340` | `0065D740` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x65e354` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00661E17` | `00661217` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x661e2b` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00662C0F` | `0066200F` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x662c1c` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00662C9E` | `0066209E` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x662cac` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00662D01` | `00662101` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x662d28` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00662E84` | `00662284` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x662e91` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006658A5` | `00664CA5` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x6658b9` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00665935` | `00664D35` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x665949` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0066B20A` | `0066A60A` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x66b21e` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0066B69D` | `0066AA9D` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x66b6b1` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0066C0C2` | `0066B4C2` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x66c0d6` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0066C30D` | `0066B70D` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x66c321` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0066D095` | `0066C495` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x66d0a3` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0066D316` | `0066C716` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x66d324` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0066DDA1` | `0066D1A1` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x66ddb5` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0066E94F` | `0066DD4F` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x66e963` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00670586` | `0066F986` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x670594` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00670716` | `0066FB16` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jae 0x670723` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006731EA` | `006725EA` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x6731fe` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00673543` | `00672943` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x673557` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00673D55` | `00673155` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x673d69` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00675A5E` | `00674E5E` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x675a72` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006760B1` | `006754B1` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x6760c5` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00676F84` | `00676384` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x676f9e` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006782C5` | `006776C5` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x6782d9` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067856D` | `0067796D` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x678581` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067969A` | `00678A9A` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jge 0x6796ae` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067B57E` | `0067A97E` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67b592` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067B5DC` | `0067A9DC` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67b5f0` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067B63A` | `0067AA3A` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67b64e` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067B693` | `0067AA93` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67b6a7` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067B744` | `0067AB44` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67b758` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067B7A3` | `0067ABA3` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67b7b7` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067C19F` | `0067B59F` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67c1ad` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067C1EA` | `0067B5EA` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67c1f8` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067C225` | `0067B625` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67c233` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067CA75` | `0067BE75` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67ca83` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067CAE0` | `0067BEE0` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67cafb` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067CB27` | `0067BF27` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67cb35` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067CBA3` | `0067BFA3` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67cbc4` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067CC3A` | `0067C03A` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jae 0x67cc47` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067CC96` | `0067C096` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67cca4` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067CCE0` | `0067C0E0` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67ccee` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067CD33` | `0067C133` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67cd41` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067CD81` | `0067C181` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67cd8f` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067CDE1` | `0067C1E1` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67cdef` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067CE55` | `0067C255` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67ce63` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067CFC9` | `0067C3C9` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67cfd7` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067D12B` | `0067C52B` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67d139` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067D5DF` | `0067C9DF` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67d5ed` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067D671` | `0067CA71` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67d681` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067D9AA` | `0067CDAA` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67d9b8` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067DA1C` | `0067CE1C` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67da2a` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067DB5D` | `0067CF5D` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jae 0x67db6a` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067DD56` | `0067D156` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67dd64` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067DDBE` | `0067D1BE` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67ddcc` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067DE33` | `0067D233` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67de41` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067DEC3` | `0067D2C3` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67ded1` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067DF0C` | `0067D30C` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67df1a` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067DF62` | `0067D362` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67df70` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067DFAB` | `0067D3AB` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67dfb9` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067E017` | `0067D417` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67e025` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067E069` | `0067D469` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67e077` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067E107` | `0067D507` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67e115` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067E16D` | `0067D56D` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67e17b` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067E227` | `0067D627` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67e235` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067E2A4` | `0067D6A4` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67e2b2` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067E30E` | `0067D70E` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67e332` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067E3AA` | `0067D7AA` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67e3b8` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067E516` | `0067D916` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67e524` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067E5B6` | `0067D9B6` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67e5c4` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067E656` | `0067DA56` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67e664` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067E83F` | `0067DC3F` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67e84d` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067E8C3` | `0067DCC3` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67e8d1` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067E924` | `0067DD24` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67e932` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067F958` | `0067ED58` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67f966` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067FC70` | `0067F070` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x67fd59` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0067FFBE` | `0067F3BE` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x67ffd2` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00680079` | `0067F479` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x68008d` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0068089A` | `0067FC9A` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jge 0x6808ae` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00681109` | `00680509` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x68111d` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00681C61` | `00681061` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x681c75` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00682763` | `00681B63` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x68277a` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006832A9` | `006826A9` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jl 0x6832cc` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006841FB` | `006835FB` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x684209` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00687F5A` | `0068735A` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x687f7a` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006882A5` | `006876A5` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x6882b9` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00688572` | `00687972` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jb 0x68858f` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006885C1` | `006879C1` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jae 0x6885ce` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00688732` | `00687B32` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jb 0x68874f` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00688775` | `00687B75` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x68878f` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006888C5` | `00687CC5` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jb 0x6888e2` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00688908` | `00687D08` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x688922` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00688FE3` | `006883E3` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x688ff7` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00689246` | `00688646` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x68925a` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00689383` | `00688783` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jae 0x689397` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00689EAB` | `006892AB` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jae 0x689ebf` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0068AA44` | `00689E44` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x68aa5e` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0068AE3C` | `0068A23C` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x68ae50` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0068C520` | `0068B920` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x68c534` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0068CC37` | `0068C037` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x68cc47` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0068DF84` | `0068D384` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x68df98` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0068EA35` | `0068DE35` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x68ea4f` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00690314` | `0068F714` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jae 0x690328` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006915F7` | `006909F7` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x69160b` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00695BC8` | `00694FC8` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x695bdc` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0069609C` | `0069549C` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jae 0x6960aa` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006965FF` | `006959FF` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x696613` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006966C3` | `00695AC3` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x6966d7` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00696E3F` | `0069623F` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x696e53` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006979F4` | `00696DF4` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x697a08` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00697AB3` | `00696EB3` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x697ac7` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00698109` | `00697509` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jae 0x69811d` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00698299` | `00697699` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jae 0x698367` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00698404` | `00697804` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x698797` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00698803` | `00697C03` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x698954` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006989DE` | `00697DDE` | `81 fa 20 4e 00 00` | `cmp edx, 0x4e20` | `jae 0x698d44` | `81 fa 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00698DE4` | `006981E4` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jae 0x698df1` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00698F3B` | `0069833B` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x698f49` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006990EA` | `006984EA` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x6990f8` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006996F4` | `00698AF4` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x699961` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `00699D5D` | `0069915D` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jae 0x699d71` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0069A9CC` | `00699DCC` | `81 fa 20 4e 00 00` | `cmp edx, 0x4e20` | `jae 0x69a9e0` | `81 fa 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0069B573` | `0069A973` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x69b587` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0069B5D9` | `0069A9D9` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x69b5ed` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0069B74D` | `0069AB4D` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x69b761` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0069BE43` | `0069B243` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x69be57` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0069BEA9` | `0069B2A9` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x69bebd` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0069C00D` | `0069B40D` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x69c021` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0069D4BB` | `0069C8BB` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x69d4d2` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0069DA34` | `0069CE34` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x69da42` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0069E62B` | `0069DA2B` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x69e63c` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0069E719` | `0069DB19` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jae 0x69e726` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0069E841` | `0069DC41` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x69e855` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0069EA04` | `0069DE04` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jae 0x69ea11` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0069EB7A` | `0069DF7A` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x69eb88` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `0069EC1D` | `0069E01D` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `jae 0x69ec2b` | `81 f9 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006A0E9F` | `006A029F` | `81 fe 20 4e 00 00` | `cmp esi, 0x4e20` | `jae 0x6a0eb3` | `81 fe 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006A0F98` | `006A0398` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jae 0x6a0fb9` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006A141C` | `006A081C` | `81 ff 20 4e 00 00` | `cmp edi, 0x4e20` | `jae 0x6a1430` | `81 ff 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006A53F3` | `006A47F3` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jge 0x6a55c9` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006A8621` | `006A7A21` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jbe 0x6a863f` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006A86B1` | `006A7AB1` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jbe 0x6a86c8` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006A9C07` | `006A9007` | `81 fa 20 4e 00 00` | `cmp edx, 0x4e20` | `jl 0x6a9c25` | `81 fa 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006AA196` | `006A9596` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jle 0x6aa1b4` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006B87BA` | `006B7BBA` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jg 0x6b87f0` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006B87CD` | `006B7BCD` | `81 7e 30 20 4e 00 00` | `cmp dword ptr [esi + 0x30], 0x4e20` | `jle 0x6b87e4` | `81 7e 30 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006B8A39` | `006B7E39` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x6b8a4e` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006B8E97` | `006B8297` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x6b8eb0` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006B9315` | `006B8715` | `3d 20 4e 00 00` | `cmp eax, 0x4e20` | `jge 0x6b932a` | `3d 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006BA504` | `006B9904` | `81 7e 30 20 4e 00 00` | `cmp dword ptr [esi + 0x30], 0x4e20` | `jg 0x6ba568` | `81 7e 30 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |
| `006BA7A7` | `006B9BA7` | `81 fb 20 4e 00 00` | `cmp ebx, 0x4e20` | `jg 0x6ba805` | `81 fb 60 ea 00 00` | ceiling 20,000 -> 60,000 (same instruction, new number) |

## 6. Family C — NPC floor (`0x2134` -> 20,001; 52 sites)

`cmp ?, 8500` rows that cluster with `20,000` (the IsNPC shape, legacy
`8500 <= v < 20000`), plus isolated below-form branches — only an NPC-floor read
jumps below `8500`. The floor moves with the band: the NPC class (static, town,
instance, monster) starts at `20_001`. The partner `0x4E20` half of each pair
lives in §5 with the same target. 40 clusters carry the
full IsNPC pair (Gate 0 counted 54).

| RVA | file off | old bytes | disasm | jcc | new bytes | rule |
|---|---|---|---|---|---|---|
| `00125CD2` | `001250D2` | `81 ff 34 21 00 00` | `cmp edi, 0x2134` | `jl 0x125d88` | `81 ff 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `001269F2` | `00125DF2` | `81 fe 34 21 00 00` | `cmp esi, 0x2134` | `jl 0x126aa8` | `81 fe 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0033C321` | `0033B721` | `81 3c 01 34 21 00 00` | `cmp dword ptr [ecx + eax], 0x2134` | `jl 0x33c3ae` | `81 3c 01 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0033C828` | `0033BC28` | `81 3c 01 34 21 00 00` | `cmp dword ptr [ecx + eax], 0x2134` | `jl 0x33c8bb` | `81 3c 01 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `00374698` | `00373A98` | `81 fe 34 21 00 00` | `cmp esi, 0x2134` | `jl 0x3746a8` | `81 fe 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `00374C4F` | `0037404F` | `81 fb 34 21 00 00` | `cmp ebx, 0x2134` | `jl 0x374c5f` | `81 fb 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `00375278` | `00374678` | `81 fe 34 21 00 00` | `cmp esi, 0x2134` | `jl 0x375288` | `81 fe 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `00375858` | `00374C58` | `81 fe 34 21 00 00` | `cmp esi, 0x2134` | `jl 0x375868` | `81 fe 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `003764EF` | `003758EF` | `81 fb 34 21 00 00` | `cmp ebx, 0x2134` | `jl 0x3764ff` | `81 fb 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `003769C8` | `00375DC8` | `81 ff 34 21 00 00` | `cmp edi, 0x2134` | `jl 0x3769d8` | `81 ff 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0037702F` | `0037642F` | `81 fb 34 21 00 00` | `cmp ebx, 0x2134` | `jl 0x37703f` | `81 fb 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0037763F` | `00376A3F` | `81 fb 34 21 00 00` | `cmp ebx, 0x2134` | `jl 0x37764f` | `81 fb 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `00377C3F` | `0037703F` | `81 fb 34 21 00 00` | `cmp ebx, 0x2134` | `jl 0x377c4f` | `81 fb 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0037823F` | `0037763F` | `81 fb 34 21 00 00` | `cmp ebx, 0x2134` | `jl 0x37824f` | `81 fb 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `00378788` | `00377B88` | `81 ff 34 21 00 00` | `cmp edi, 0x2134` | `jl 0x378798` | `81 ff 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `00379028` | `00378428` | `81 fe 34 21 00 00` | `cmp esi, 0x2134` | `jl 0x379038` | `81 fe 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `00379698` | `00378A98` | `81 ff 34 21 00 00` | `cmp edi, 0x2134` | `jl 0x3796a8` | `81 ff 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `00379E6F` | `0037926F` | `81 fb 34 21 00 00` | `cmp ebx, 0x2134` | `jl 0x379e7f` | `81 fb 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0037A4DF` | `003798DF` | `81 fb 34 21 00 00` | `cmp ebx, 0x2134` | `jl 0x37a4ef` | `81 fb 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0037AA98` | `00379E98` | `81 ff 34 21 00 00` | `cmp edi, 0x2134` | `jl 0x37aaa8` | `81 ff 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0037B0C8` | `0037A4C8` | `81 fe 34 21 00 00` | `cmp esi, 0x2134` | `jl 0x37b0d8` | `81 fe 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0037B55C` | `0037A95C` | `81 fe 34 21 00 00` | `cmp esi, 0x2134` | `jl 0x37b56c` | `81 fe 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0037BBC8` | `0037AFC8` | `81 fe 34 21 00 00` | `cmp esi, 0x2134` | `jl 0x37bbd8` | `81 fe 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0037C4D7` | `0037B8D7` | `81 fb 34 21 00 00` | `cmp ebx, 0x2134` | `jl 0x37c4e7` | `81 fb 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0037CB88` | `0037BF88` | `81 fe 34 21 00 00` | `cmp esi, 0x2134` | `jl 0x37cb98` | `81 fe 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0038B388` | `0038A788` | `81 fe 34 21 00 00` | `cmp esi, 0x2134` | `jl 0x38b398` | `81 fe 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0038B998` | `0038AD98` | `81 ff 34 21 00 00` | `cmp edi, 0x2134` | `jl 0x38b9a8` | `81 ff 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0038BFF8` | `0038B3F8` | `81 fe 34 21 00 00` | `cmp esi, 0x2134` | `jl 0x38c008` | `81 fe 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0038C54F` | `0038B94F` | `81 fb 34 21 00 00` | `cmp ebx, 0x2134` | `jl 0x38c55f` | `81 fb 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0038CD88` | `0038C188` | `81 fb 34 21 00 00` | `cmp ebx, 0x2134` | `jl 0x38cd98` | `81 fb 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0038D3EF` | `0038C7EF` | `81 fb 34 21 00 00` | `cmp ebx, 0x2134` | `jl 0x38d3ff` | `81 fb 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0038DAF8` | `0038CEF8` | `81 ff 34 21 00 00` | `cmp edi, 0x2134` | `jl 0x38db08` | `81 ff 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0038E06F` | `0038D46F` | `81 fb 34 21 00 00` | `cmp ebx, 0x2134` | `jl 0x38e07f` | `81 fb 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0038E63F` | `0038DA3F` | `81 fb 34 21 00 00` | `cmp ebx, 0x2134` | `jl 0x38e64f` | `81 fb 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0038F53F` | `0038E93F` | `81 fb 34 21 00 00` | `cmp ebx, 0x2134` | `jl 0x38f54f` | `81 fb 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0038FCC8` | `0038F0C8` | `81 ff 34 21 00 00` | `cmp edi, 0x2134` | `jl 0x38fcd8` | `81 ff 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `003AF39A` | `003AE79A` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x3af481` | `3d 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `003CD887` | `003CCC87` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x3cd8e8` | `3d 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `003D6647` | `003D5A47` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x3d67c7` | `3d 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `003EB8F2` | `003EACF2` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x3eb912` | `3d 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `003F1020` | `003F0420` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x3f105a` | `3d 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `003FA148` | `003F9548` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x3fa55f` | `3d 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `00412847` | `00411C47` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x412886` | `3d 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `00412E14` | `00412214` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x412e44` | `3d 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0041319B` | `0041259B` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x4131cb` | `3d 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `004134DF` | `004128DF` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x41353e` | `3d 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0041363F` | `00412A3F` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x41366c` | `3d 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0041EA56` | `0041DE56` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x41eb5f` | `3d 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `00420069` | `0041F469` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x420101` | `3d 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `00426B55` | `00425F55` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x426d31` | `3d 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `0042FC4F` | `0042F04F` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x42fd77` | `3d 21 4e 00 00` | NPC floor 8,500 -> 20,001 |
| `00580E8B` | `0058028B` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x580ebf` | `3d 21 4e 00 00` | NPC floor 8,500 -> 20,001 |

## 7. Family C — instance-NPC ceiling (`0x2134` + sibling `0xBB8`; 28 sites) — APPROVED: Option A

**Resolved (Cat, 2026-10-20): Option A.** Instance NPCs are the whole band
`20_001..=29_999`, so the classifier's ceiling divide moves `8,500 -> 30,000` —
non-strict rows take `0x7530`, strict rows `0x752F` (29,999). The floor half of
each pair already sits in §4 (`0xBB8 -> 0x4E21`), unchanged by this choice.
Within the band the client can no longer range-separate instance from
town/static NPCs: instance-vs-town behaviour keys off the kind table — the
merge's one condition, recorded here with the approval.

Row notes: the stock upper bound is written against `8500` — there is no
literal `8499` anywhere in this binary. Three-immediate chains were placed here
by branch target (the instance condition shares the floor's fail label); a
chain whose `8,500` branch lands elsewhere is §9. Option B (sub-slice
`20_001..=24_999`, ceiling `25_000` = `0x61A8`/`0x61A7`) was considered and
**not taken**.

| RVA | file off | old bytes | disasm | jcc | new bytes | rule |
|---|---|---|---|---|---|---|
| `000E7D2E` | `000E712E` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0xe7dab` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `001058E4` | `00104CE4` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x1058f3` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `001150CE` | `001144CE` | `81 fe 34 21 00 00` | `cmp esi, 0x2134` | `jge 0x1150dd` | `81 fe 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `002D4D55` | `002D4155` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x2d4d63` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `002D4F58` | `002D4358` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x2d4f66` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `002D512E` | `002D452E` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x2d513c` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `002D52A8` | `002D46A8` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x2d52b6` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `002D5455` | `002D4855` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x2d5463` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `0033AC34` | `0033A034` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x33ac43` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `0035E0D0` | `0035D4D0` | `81 ff 34 21 00 00` | `cmp edi, 0x2134` | `jb 0x35e155` | `81 ff 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `003D08AE` | `003CFCAE` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x3d08e2` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `003D7294` | `003D6694` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x3d72a5` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `003D7985` | `003D6D85` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x3d7996` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `003D7C68` | `003D7068` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x3d7c78` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `003EAB02` | `003E9F02` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x3eab13` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `003F2E1D` | `003F221D` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x3f2e2a` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `00414035` | `00413435` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x4140b6` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `0042A3DE` | `004297DE` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x42a4f1` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `0042A41E` | `0042981E` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x42a4f1` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `004D40AF` | `004D34AF` | `81 f9 34 21 00 00` | `cmp ecx, 0x2134` | `jge 0x4d410e` | `81 f9 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `004D452F` | `004D392F` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x4d4577` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `004D4C00` | `004D4000` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x4d4c42` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `00574AC1` | `00573EC1` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x574add` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `005866F3` | `00585AF3` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x58670f` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `00591B2B` | `00590F2B` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x591b39` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `0060DB71` | `0060CF71` | `81 fe 34 21 00 00` | `cmp esi, 0x2134` | `jge 0x60db80` | `81 fe 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `0060F42D` | `0060E82D` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x60f43b` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |
| `0061B704` | `0061AB04` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x61b714` | `3d 30 75 00 00` | approved A: instance ceiling 8,500 -> 30,000 |

## 8. Excluded — `0x7530` (30,000) sites are item tables, NOT char ids (1 imm16 + 372 dword hits)

Gate 0 established that every 30,000-class constant in this binary is
`MAX_ITEM_UNIQUE` (item-unique tables) or the ITZ ring — never a char id. They are
listed for completeness so a future sweep does not mistake them for char-table
guards. **Do not patch them.**

| RVA | file off | old bytes | disasm | jcc | new bytes | rule |
|---|---|---|---|---|---|---|
| `000383C5` | `000377C5` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jae 0x3845b` | `TBD` | manual review - section 9 |
| `0004028B` | `0003F68B` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jl 0x4029a` | `TBD` | manual review - section 9 |
| `000473DA` | `000467DA` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jg 0x47792` | `TBD` | manual review - section 9 |
| `0005D722` | `0005CB22` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x5d817` | `TBD` | manual review - section 9 |
| `00061889` | `00060C89` | `26 3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x618b0` | `TBD` | manual review - section 9 |
| `000618CA` | `00060CCA` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x618de` | `TBD` | manual review - section 9 |
| `00074C21` | `00074021` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x74c2d` | `TBD` | manual review - section 9 |
| `000768FE` | `00075CFE` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x769ca` | `TBD` | manual review - section 9 |
| `00079C54` | `00079054` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `je 0x79c81` | `TBD` | manual review - section 9 |
| `0007C24A` | `0007B64A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jle 0x7c310` | `TBD` | manual review - section 9 |
| `000C6FEA` | `000C63EA` | `81 fb 30 75 00 00` | `cmp ebx, 0x7530` | `jge 0xc7224` | `TBD` | manual review - section 9 |
| `000CCF36` | `000CC336` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0xccf42` | `TBD` | manual review - section 9 |
| `000D0797` | `000CFB97` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0xd07a3` | `TBD` | manual review - section 9 |
| `000D13F8` | `000D07F8` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0xd1404` | `TBD` | manual review - section 9 |
| `000D1BC2` | `000D0FC2` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0xd1d0f` | `TBD` | manual review - section 9 |
| `000DF50C` | `000DE90C` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0xdf668` | `TBD` | manual review - section 9 |
| `000F83D1` | `000F77D1` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0xf83ec` | `TBD` | manual review - section 9 |
| `000F8615` | `000F7A15` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0xf8d63` | `TBD` | manual review - section 9 |
| `000F877E` | `000F7B7E` | `64 81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0xf8798` | `TBD` | manual review - section 9 |
| `000F8905` | `000F7D05` | `81 fb 30 75 00 00` | `cmp ebx, 0x7530` | `jge 0xf89bd` | `TBD` | manual review - section 9 |
| `000F8983` | `000F7D83` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0xf89c3` | `TBD` | manual review - section 9 |
| `000F9949` | `000F8D49` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0xf9930` | `TBD` | manual review - section 9 |
| `000F9E3A` | `000F923A` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0xf9edf` | `TBD` | manual review - section 9 |
| `000F9EA5` | `000F92A5` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0xf9edf` | `TBD` | manual review - section 9 |
| `000FC7F1` | `000FBBF1` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0xfc80c` | `TBD` | manual review - section 9 |
| `000FCA16` | `000FBE16` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0xfcbd9` | `TBD` | manual review - section 9 |
| `000FCB2F` | `000FBF2F` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0xfcb48` | `TBD` | manual review - section 9 |
| `000FD499` | `000FC899` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0xfd480` | `TBD` | manual review - section 9 |
| `000FFE1A` | `000FF21A` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0xffe26` | `TBD` | manual review - section 9 |
| `0010098D` | `000FFD8D` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x100dd5` | `TBD` | manual review - section 9 |
| `00100AB4` | `000FFEB4` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x100ac0` | `TBD` | manual review - section 9 |
| `00100B8F` | `000FFF8F` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x100ba8` | `TBD` | manual review - section 9 |
| `00101079` | `00100479` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x101060` | `TBD` | manual review - section 9 |
| `001562A3` | `001556A3` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x1563aa` | `TBD` | manual review - section 9 |
| `001566F0` | `00155AF0` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x1567b5` | `TBD` | manual review - section 9 |
| `001707C0` | `0016FBC0` | `64 81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x1707da` | `TBD` | manual review - section 9 |
| `00171A13` | `00170E13` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jl 0x171a32` | `TBD` | manual review - section 9 |
| `0017B6BF` | `0017AABF` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x17b881` | `TBD` | manual review - section 9 |
| `0017B94E` | `0017AD4E` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x17b9f6` | `TBD` | manual review - section 9 |
| `00188A7E` | `00187E7E` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x188c3a` | `TBD` | manual review - section 9 |
| `0018A5BA` | `001899BA` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x18a834` | `TBD` | manual review - section 9 |
| `0018A679` | `00189A79` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x18a819` | `TBD` | manual review - section 9 |
| `001B02E7` | `001AF6E7` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jle 0x1b032d` | `TBD` | manual review - section 9 |
| `001CC041` | `001CB441` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x1cc313` | `TBD` | manual review - section 9 |
| `001CDF4A` | `001CD34A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x1ce00a` | `TBD` | manual review - section 9 |
| `001CE0AF` | `001CD4AF` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x1ce16e` | `TBD` | manual review - section 9 |
| `001CE277` | `001CD677` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jl 0x1ce286` | `TBD` | manual review - section 9 |
| `001CEE0C` | `001CE20C` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x1cef01` | `TBD` | manual review - section 9 |
| `001D257A` | `001D197A` | `64 81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jl 0x1d2594` | `TBD` | manual review - section 9 |
| `001D45AC` | `001D39AC` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x1d4b46` | `TBD` | manual review - section 9 |
| `001EA8F9` | `001E9CF9` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x1eb18d` | `TBD` | manual review - section 9 |
| `001EABBC` | `001E9FBC` | `81 fb 30 75 00 00` | `cmp ebx, 0x7530` | `jle 0x1eac2c` | `TBD` | manual review - section 9 |
| `001EF2B1` | `001EE6B1` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x1ef725` | `TBD` | manual review - section 9 |
| `001F068E` | `001EFA8E` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jl 0x1f071a` | `TBD` | manual review - section 9 |
| `001F3050` | `001F2450` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jl 0x1f3062` | `TBD` | manual review - section 9 |
| `001F6D5A` | `001F615A` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x1f748b` | `TBD` | manual review - section 9 |
| `001F6FD9` | `001F63D9` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x1f748b` | `TBD` | manual review - section 9 |
| `001F729F` | `001F669F` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x1f7485` | `TBD` | manual review - section 9 |
| `00207652` | `00206A52` | `81 fb 30 75 00 00` | `cmp ebx, 0x7530` | `jge 0x20797c` | `TBD` | manual review - section 9 |
| `002161ED` | `002155ED` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x21628c` | `TBD` | manual review - section 9 |
| `0021671C` | `00215B1C` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x2167a1` | `TBD` | manual review - section 9 |
| `0021E004` | `0021D404` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jl 0x21e01d` | `TBD` | manual review - section 9 |
| `00222917` | `00221D17` | `67 81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x22297f` | `TBD` | manual review - section 9 |
| `002229AF` | `00221DAF` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x222a12` | `TBD` | manual review - section 9 |
| `00222A5B` | `00221E5B` | `65 81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x222ac1` | `TBD` | manual review - section 9 |
| `00222C7C` | `0022207C` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x222cdf` | `TBD` | manual review - section 9 |
| `00222D0F` | `0022210F` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x222d72` | `TBD` | manual review - section 9 |
| `00222DBB` | `002221BB` | `65 81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x222e21` | `TBD` | manual review - section 9 |
| `0022B055` | `0022A455` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x22b4c3` | `TBD` | manual review - section 9 |
| `0022B370` | `0022A770` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `push` | `TBD` | manual review - section 9 |
| `0022B52D` | `0022A92D` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x22b696` | `TBD` | manual review - section 9 |
| `0022BB38` | `0022AF38` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x22bb44` | `TBD` | manual review - section 9 |
| `0022BD8B` | `0022B18B` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x22bd97` | `TBD` | manual review - section 9 |
| `0022D0CA` | `0022C4CA` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x22d0d6` | `TBD` | manual review - section 9 |
| `0022D42E` | `0022C82E` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x22d43a` | `TBD` | manual review - section 9 |
| `0025468B` | `00253A8B` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x254b9d` | `TBD` | manual review - section 9 |
| `002547E2` | `00253BE2` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jbe 0x25483b` | `TBD` | manual review - section 9 |
| `00254F73` | `00254373` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jbe 0x254fd8` | `TBD` | manual review - section 9 |
| `00255720` | `00254B20` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jbe 0x25577a` | `TBD` | manual review - section 9 |
| `00255C8F` | `0025508F` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jbe 0x255ce9` | `TBD` | manual review - section 9 |
| `0028B613` | `0028AA13` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x28b84d` | `TBD` | manual review - section 9 |
| `0028CC3A` | `0028C03A` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x28cc46` | `TBD` | manual review - section 9 |
| `0028D3BA` | `0028C7BA` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x28d57c` | `TBD` | manual review - section 9 |
| `002A2E4C` | `002A224C` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jle 0x2a2e94` | `TBD` | manual review - section 9 |
| `002B4579` | `002B3979` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x2b479a` | `TBD` | manual review - section 9 |
| `002BCBFE` | `002BBFFE` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jle 0x2bceb7` | `TBD` | manual review - section 9 |
| `002E4D1E` | `002E411E` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `mov` | `TBD` | manual review - section 9 |
| `00302E6E` | `0030226E` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x302f2c` | `TBD` | manual review - section 9 |
| `003033DE` | `003027DE` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x303469` | `TBD` | manual review - section 9 |
| `0030716D` | `0030656D` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x307b9c` | `TBD` | manual review - section 9 |
| `003088A1` | `00307CA1` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x309049` | `TBD` | manual review - section 9 |
| `0030B7BA` | `0030ABBA` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x30b949` | `TBD` | manual review - section 9 |
| `0030BA6E` | `0030AE6E` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jl 0x30ba79` | `TBD` | manual review - section 9 |
| `0030BADA` | `0030AEDA` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jl 0x30baec` | `TBD` | manual review - section 9 |
| `0030BC1D` | `0030B01D` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x30c658` | `TBD` | manual review - section 9 |
| `00315083` | `00314483` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x31545e` | `TBD` | manual review - section 9 |
| `00315BF6` | `00314FF6` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x315d6e` | `TBD` | manual review - section 9 |
| `00315DB2` | `003151B2` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x31637a` | `TBD` | manual review - section 9 |
| `003165CD` | `003159CD` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x317008` | `TBD` | manual review - section 9 |
| `00317A39` | `00316E39` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jle 0x317a6b` | `TBD` | manual review - section 9 |
| `003279ED` | `00326DED` | `36 81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x3279fa` | `TBD` | manual review - section 9 |
| `00338F69` | `00338369` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x338f75` | `TBD` | manual review - section 9 |
| `0033DD8B` | `0033D18B` | `81 fb 30 75 00 00` | `cmp ebx, 0x7530` | `jge 0x33de29` | `TBD` | manual review - section 9 |
| `003486D2` | `00347AD2` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x3488e9` | `TBD` | manual review - section 9 |
| `00355C5C` | `0035505C` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x355dab` | `TBD` | manual review - section 9 |
| `0035996E` | `00358D6E` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x3599c3` | `TBD` | manual review - section 9 |
| `00359A43` | `00358E43` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x359a8e` | `TBD` | manual review - section 9 |
| `0035A1CD` | `003595CD` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x35a17f` | `TBD` | manual review - section 9 |
| `0035B1E4` | `0035A5E4` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x35b245` | `TBD` | manual review - section 9 |
| `0035BC99` | `0035B099` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x35bcdb` | `TBD` | manual review - section 9 |
| `0035BD5F` | `0035B15F` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x35bd9f` | `TBD` | manual review - section 9 |
| `0035C130` | `0035B530` | `81 ff 30 75 00 00` | `cmp edi, 0x7530` | `jge 0x35c10e` | `TBD` | manual review - section 9 |
| `0036B557` | `0036A957` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jle 0x36bc9b` | `TBD` | manual review - section 9 |
| `0036CAD7` | `0036BED7` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jle 0x36caea` | `TBD` | manual review - section 9 |
| `0036CB0C` | `0036BF0C` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jg 0x36cb46` | `TBD` | manual review - section 9 |
| `0036DA28` | `0036CE28` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jle 0x36da3a` | `TBD` | manual review - section 9 |
| `0036E60E` | `0036DA0E` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x36e689` | `TBD` | manual review - section 9 |
| `0036EA8D` | `0036DE8D` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jle 0x36ea9f` | `TBD` | manual review - section 9 |
| `0036EE2B` | `0036E22B` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x36ee96` | `TBD` | manual review - section 9 |
| `00391293` | `00390693` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x3913ee` | `TBD` | manual review - section 9 |
| `00391991` | `00390D91` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jl 0x39199d` | `TBD` | manual review - section 9 |
| `00391BB7` | `00390FB7` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x391cd0` | `TBD` | manual review - section 9 |
| `00392039` | `00391439` | `81 ff 30 75 00 00` | `cmp edi, 0x7530` | `jl 0x392045` | `TBD` | manual review - section 9 |
| `00392542` | `00391942` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x392500` | `TBD` | manual review - section 9 |
| `00392852` | `00391C52` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jl 0x3925b1` | `TBD` | manual review - section 9 |
| `003CA658` | `003C9A58` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x3ca6ee` | `TBD` | manual review - section 9 |
| `003CBBC5` | `003CAFC5` | `81 ff 30 75 00 00` | `cmp edi, 0x7530` | `jae 0x3cbc2b` | `TBD` | manual review - section 9 |
| `003CBD19` | `003CB119` | `81 ff 30 75 00 00` | `cmp edi, 0x7530` | `jae 0x3cbd7b` | `TBD` | manual review - section 9 |
| `003CBDC5` | `003CB1C5` | `81 ff 30 75 00 00` | `cmp edi, 0x7530` | `jge 0x3cbdf4` | `TBD` | manual review - section 9 |
| `003CBE7E` | `003CB27E` | `81 ff 30 75 00 00` | `cmp edi, 0x7530` | `jge 0x3cbead` | `TBD` | manual review - section 9 |
| `003CBF35` | `003CB335` | `65 81 ff 30 75 00 00` | `cmp edi, 0x7530` | `jge 0x3cbf9b` | `TBD` | manual review - section 9 |
| `003CC02D` | `003CB42D` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x3cc121` | `TBD` | manual review - section 9 |
| `003CC0BC` | `003CB4BC` | `64 81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jae 0x3cc121` | `TBD` | manual review - section 9 |
| `003CC2A0` | `003CB6A0` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x3cc363` | `TBD` | manual review - section 9 |
| `003CC330` | `003CB730` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jae 0x3cc363` | `TBD` | manual review - section 9 |
| `003DFFFD` | `003DF3FD` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x3e008b` | `TBD` | manual review - section 9 |
| `003E0266` | `003DF666` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x3e04c5` | `TBD` | manual review - section 9 |
| `003E1AB0` | `003E0EB0` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x3e1ca6` | `TBD` | manual review - section 9 |
| `003F4C01` | `003F4001` | `81 ff 30 75 00 00` | `cmp edi, 0x7530` | `jge 0x3f4db8` | `TBD` | manual review - section 9 |
| `003F4E9F` | `003F429F` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x3f5048` | `TBD` | manual review - section 9 |
| `003F5160` | `003F4560` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x3f5350` | `TBD` | manual review - section 9 |
| `003F560B` | `003F4A0B` | `81 fb 30 75 00 00` | `cmp ebx, 0x7530` | `jge 0x3f5647` | `TBD` | manual review - section 9 |
| `003F5815` | `003F4C15` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x3f6235` | `TBD` | manual review - section 9 |
| `003F6046` | `003F5446` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jle 0x3f6067` | `TBD` | manual review - section 9 |
| `003F75C7` | `003F69C7` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x3f75d3` | `TBD` | manual review - section 9 |
| `003F7607` | `003F6A07` | `81 ff 30 75 00 00` | `cmp edi, 0x7530` | `jge 0x3f7a8c` | `TBD` | manual review - section 9 |
| `003F9211` | `003F8611` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x3f934d` | `TBD` | manual review - section 9 |
| `003F937D` | `003F877D` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jl 0x3f9389` | `TBD` | manual review - section 9 |
| `003FA1D3` | `003F95D3` | `81 ff 30 75 00 00` | `cmp edi, 0x7530` | `jge 0x3fa543` | `TBD` | manual review - section 9 |
| `003FA7F0` | `003F9BF0` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jl 0x3fa7fc` | `TBD` | manual review - section 9 |
| `003FCF4C` | `003FC34C` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x40ca18` | `TBD` | manual review - section 9 |
| `00415623` | `00414A23` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jbe 0x41563d` | `TBD` | manual review - section 9 |
| `00415650` | `00414A50` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jbe 0x415760` | `TBD` | manual review - section 9 |
| `00416BC7` | `00415FC7` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jl 0x416be0` | `TBD` | manual review - section 9 |
| `004170A7` | `004164A7` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jl 0x417503` | `TBD` | manual review - section 9 |
| `0041752F` | `0041692F` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jl 0x41768a` | `TBD` | manual review - section 9 |
| `00417A8D` | `00416E8D` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jl 0x418039` | `TBD` | manual review - section 9 |
| `0041AA56` | `00419E56` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jle 0x41ab04` | `TBD` | manual review - section 9 |
| `0041F260` | `0041E660` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x41f374` | `TBD` | manual review - section 9 |
| `00420FE2` | `004203E2` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jl 0x4211de` | `TBD` | manual review - section 9 |
| `00429979` | `00428D79` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x429b70` | `TBD` | manual review - section 9 |
| `00429CFB` | `004290FB` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x429e2a` | `TBD` | manual review - section 9 |
| `0042C22B` | `0042B62B` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x42c2c4` | `TBD` | manual review - section 9 |
| `0042D20B` | `0042C60B` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x42d2ee` | `TBD` | manual review - section 9 |
| `0042E63B` | `0042DA3B` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x42e6d7` | `TBD` | manual review - section 9 |
| `0042F5E2` | `0042E9E2` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x42f658` | `TBD` | manual review - section 9 |
| `00440BF0` | `0043FFF0` | `81 ff 30 75 00 00` | `cmp edi, 0x7530` | `jge 0x440cc9` | `TBD` | manual review - section 9 |
| `00449203` | `00448603` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x449244` | `TBD` | manual review - section 9 |
| `0044930C` | `0044870C` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jae 0x4494b7` | `TBD` | manual review - section 9 |
| `0045E84B` | `0045DC4B` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x45e857` | `TBD` | manual review - section 9 |
| `0046205B` | `0046145B` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x462074` | `TBD` | manual review - section 9 |
| `0046A443` | `00469843` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x46a64e` | `TBD` | manual review - section 9 |
| `0046A814` | `00469C14` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x46a77b` | `TBD` | manual review - section 9 |
| `0047167D` | `00470A7D` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4716dd` | `TBD` | manual review - section 9 |
| `00471809` | `00470C09` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x47684c` | `TBD` | manual review - section 9 |
| `00476A5C` | `00475E5C` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x47700b` | `TBD` | manual review - section 9 |
| `004775CF` | `004769CF` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x47789d` | `TBD` | manual review - section 9 |
| `004778F1` | `00476CF1` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x477ccb` | `TBD` | manual review - section 9 |
| `00477A06` | `00476E06` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x477ae2` | `TBD` | manual review - section 9 |
| `00477BE7` | `00476FE7` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x477ca6` | `TBD` | manual review - section 9 |
| `00477CF4` | `004770F4` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x477e56` | `TBD` | manual review - section 9 |
| `004786A1` | `00477AA1` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x47887b` | `TBD` | manual review - section 9 |
| `00493E3B` | `0049323B` | `81 fb 30 75 00 00` | `cmp ebx, 0x7530` | `jge 0x493d80` | `TBD` | manual review - section 9 |
| `00494028` | `00493428` | `81 fb 30 75 00 00` | `cmp ebx, 0x7530` | `jge 0x49407d` | `TBD` | manual review - section 9 |
| `00498581` | `00497981` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x4986ff` | `TBD` | manual review - section 9 |
| `004992E6` | `004986E6` | `81 fb 30 75 00 00` | `cmp ebx, 0x7530` | `jge 0x499244` | `TBD` | manual review - section 9 |
| `004997D3` | `00498BD3` | `81 fb 30 75 00 00` | `cmp ebx, 0x7530` | `jge 0x499725` | `TBD` | manual review - section 9 |
| `0049AF7E` | `0049A37E` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x49af8a` | `TBD` | manual review - section 9 |
| `0049B34D` | `0049A74D` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jl 0x49b358` | `TBD` | manual review - section 9 |
| `0049B7B9` | `0049ABB9` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x49b81f` | `TBD` | manual review - section 9 |
| `0049B842` | `0049AC42` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x49b8ae` | `TBD` | manual review - section 9 |
| `0049BB07` | `0049AF07` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x49c1d5` | `TBD` | manual review - section 9 |
| `0049C2E0` | `0049B6E0` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jl 0x49c2ec` | `TBD` | manual review - section 9 |
| `0049CD25` | `0049C125` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jl 0x49cd30` | `TBD` | manual review - section 9 |
| `0049DB3C` | `0049CF3C` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x49dbb2` | `TBD` | manual review - section 9 |
| `004A0788` | `0049FB88` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x4a0794` | `TBD` | manual review - section 9 |
| `004A0A7D` | `0049FE7D` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x4a0aec` | `TBD` | manual review - section 9 |
| `004A0C37` | `004A0037` | `81 ff 30 75 00 00` | `cmp edi, 0x7530` | `jl 0x4a0c43` | `TBD` | manual review - section 9 |
| `004A1D50` | `004A1150` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a1fd0` | `TBD` | manual review - section 9 |
| `004A39A6` | `004A2DA6` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x4a39a2` | `TBD` | manual review - section 9 |
| `004A3A18` | `004A2E18` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a3aa3` | `TBD` | manual review - section 9 |
| `004A3A36` | `004A2E36` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x4a3aa3` | `TBD` | manual review - section 9 |
| `004A3B6A` | `004A2F6A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a3b9a` | `TBD` | manual review - section 9 |
| `004A3C8F` | `004A308F` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a3cff` | `TBD` | manual review - section 9 |
| `004A3E55` | `004A3255` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a4119` | `TBD` | manual review - section 9 |
| `004A4169` | `004A3569` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a42c1` | `TBD` | manual review - section 9 |
| `004A4480` | `004A3880` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a450f` | `TBD` | manual review - section 9 |
| `004A49A7` | `004A3DA7` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a4cb9` | `TBD` | manual review - section 9 |
| `004A5009` | `004A4409` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a508a` | `TBD` | manual review - section 9 |
| `004A530A` | `004A470A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a535d` | `TBD` | manual review - section 9 |
| `004A5431` | `004A4831` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a547e` | `TBD` | manual review - section 9 |
| `004A62EC` | `004A56EC` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x4a6364` | `TBD` | manual review - section 9 |
| `004A63A3` | `004A57A3` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x4a644e` | `TBD` | manual review - section 9 |
| `004A696A` | `004A5D6A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a6a26` | `TBD` | manual review - section 9 |
| `004A86AA` | `004A7AAA` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a86f7` | `TBD` | manual review - section 9 |
| `004A8713` | `004A7B13` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a8792` | `TBD` | manual review - section 9 |
| `004A87B3` | `004A7BB3` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a8832` | `TBD` | manual review - section 9 |
| `004A8B1C` | `004A7F1C` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a8b69` | `TBD` | manual review - section 9 |
| `004A8C3C` | `004A803C` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a8c87` | `TBD` | manual review - section 9 |
| `004A8E99` | `004A8299` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a9049` | `TBD` | manual review - section 9 |
| `004A906C` | `004A846C` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a90bb` | `TBD` | manual review - section 9 |
| `004A90E2` | `004A84E2` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4a933c` | `TBD` | manual review - section 9 |
| `004A9F63` | `004A9363` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4aa080` | `TBD` | manual review - section 9 |
| `004A9FEC` | `004A93EC` | `81 fb 30 75 00 00` | `cmp ebx, 0x7530` | `jge 0x4aa195` | `TBD` | manual review - section 9 |
| `004AA5FA` | `004A99FA` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4aa64a` | `TBD` | manual review - section 9 |
| `004AAB8B` | `004A9F8B` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4aabe4` | `TBD` | manual review - section 9 |
| `004AAD2B` | `004AA12B` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4aad7e` | `TBD` | manual review - section 9 |
| `004AAE2A` | `004AA22A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4aaed8` | `TBD` | manual review - section 9 |
| `004AAEFD` | `004AA2FD` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4aaf8e` | `TBD` | manual review - section 9 |
| `004AB09F` | `004AA49F` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ab174` | `TBD` | manual review - section 9 |
| `004AB98A` | `004AAD8A` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x4ab9af` | `TBD` | manual review - section 9 |
| `004AB9CF` | `004AADCF` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x4aba5e` | `TBD` | manual review - section 9 |
| `004ABAAB` | `004AAEAB` | `81 ff 30 75 00 00` | `cmp edi, 0x7530` | `jge 0x4abaeb` | `TBD` | manual review - section 9 |
| `004ABB06` | `004AAF06` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x4ac0da` | `TBD` | manual review - section 9 |
| `004AC0EA` | `004AB4EA` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ac13a` | `TBD` | manual review - section 9 |
| `004AC14A` | `004AB54A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ac19a` | `TBD` | manual review - section 9 |
| `004AC1AA` | `004AB5AA` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ac1fa` | `TBD` | manual review - section 9 |
| `004AC20A` | `004AB60A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ac25a` | `TBD` | manual review - section 9 |
| `004AC26A` | `004AB66A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ac2ba` | `TBD` | manual review - section 9 |
| `004AC2CA` | `004AB6CA` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ac31a` | `TBD` | manual review - section 9 |
| `004AC32A` | `004AB72A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ac37a` | `TBD` | manual review - section 9 |
| `004AC38A` | `004AB78A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ac3da` | `TBD` | manual review - section 9 |
| `004AC3EA` | `004AB7EA` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ac43a` | `TBD` | manual review - section 9 |
| `004AC44A` | `004AB84A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ac49a` | `TBD` | manual review - section 9 |
| `004AC4AA` | `004AB8AA` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ac4fa` | `TBD` | manual review - section 9 |
| `004AC50A` | `004AB90A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ac55a` | `TBD` | manual review - section 9 |
| `004AC56A` | `004AB96A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ac5ba` | `TBD` | manual review - section 9 |
| `004AC5CA` | `004AB9CA` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ac61a` | `TBD` | manual review - section 9 |
| `004AC62A` | `004ABA2A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ac67a` | `TBD` | manual review - section 9 |
| `004AC68A` | `004ABA8A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ac6da` | `TBD` | manual review - section 9 |
| `004ACFA4` | `004AC3A4` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ad1db` | `TBD` | manual review - section 9 |
| `004AD2E3` | `004AC6E3` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ad3dc` | `TBD` | manual review - section 9 |
| `004AD522` | `004AC922` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jle 0x4ad591` | `TBD` | manual review - section 9 |
| `004AD9E6` | `004ACDE6` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jle 0x4adac7` | `TBD` | manual review - section 9 |
| `004AE6A1` | `004ADAA1` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ae797` | `TBD` | manual review - section 9 |
| `004AE8B4` | `004ADCB4` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x4ae893` | `TBD` | manual review - section 9 |
| `004AEF98` | `004AE398` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jle 0x4aefb1` | `TBD` | manual review - section 9 |
| `004AF0CB` | `004AE4CB` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jle 0x4af131` | `TBD` | manual review - section 9 |
| `004AF402` | `004AE802` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4af470` | `TBD` | manual review - section 9 |
| `004AF53A` | `004AE93A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4af58f` | `TBD` | manual review - section 9 |
| `004AF970` | `004AED70` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4afa15` | `TBD` | manual review - section 9 |
| `004AFA31` | `004AEE31` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4afa84` | `TBD` | manual review - section 9 |
| `004AFAA1` | `004AEEA1` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4afaf4` | `TBD` | manual review - section 9 |
| `004AFB11` | `004AEF11` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4afb64` | `TBD` | manual review - section 9 |
| `004AFFAA` | `004AF3AA` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b0000` | `TBD` | manual review - section 9 |
| `004B001A` | `004AF41A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b0070` | `TBD` | manual review - section 9 |
| `004B00CF` | `004AF4CF` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b0138` | `TBD` | manual review - section 9 |
| `004B0691` | `004AFA91` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b06e0` | `TBD` | manual review - section 9 |
| `004B0B91` | `004AFF91` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b0ca6` | `TBD` | manual review - section 9 |
| `004B0CF8` | `004B00F8` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jl 0x4b0d03` | `TBD` | manual review - section 9 |
| `004B0E3A` | `004B023A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b0e90` | `TBD` | manual review - section 9 |
| `004B0EAA` | `004B02AA` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b0efa` | `TBD` | manual review - section 9 |
| `004B0F0A` | `004B030A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b0f60` | `TBD` | manual review - section 9 |
| `004B105A` | `004B045A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b10aa` | `TBD` | manual review - section 9 |
| `004B10BA` | `004B04BA` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b1110` | `TBD` | manual review - section 9 |
| `004B144A` | `004B084A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b149c` | `TBD` | manual review - section 9 |
| `004B14BA` | `004B08BA` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b1510` | `TBD` | manual review - section 9 |
| `004B1537` | `004B0937` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x4b15d9` | `TBD` | manual review - section 9 |
| `004B15FA` | `004B09FA` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b1650` | `TBD` | manual review - section 9 |
| `004B1A6A` | `004B0E6A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b1ab8` | `TBD` | manual review - section 9 |
| `004B1ADB` | `004B0EDB` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b1b06` | `TBD` | manual review - section 9 |
| `004B1AFB` | `004B0EFB` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jl 0x4b1bc4` | `TBD` | manual review - section 9 |
| `004B1C4C` | `004B104C` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b1db8` | `TBD` | manual review - section 9 |
| `004B1FA1` | `004B13A1` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b2046` | `TBD` | manual review - section 9 |
| `004B2060` | `004B1460` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jle 0x4b2076` | `TBD` | manual review - section 9 |
| `004B20DE` | `004B14DE` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b2153` | `TBD` | manual review - section 9 |
| `004B2234` | `004B1634` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b22f1` | `TBD` | manual review - section 9 |
| `004B2325` | `004B1725` | `67 3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b238d` | `TBD` | manual review - section 9 |
| `004B23B4` | `004B17B4` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x4b2473` | `TBD` | manual review - section 9 |
| `004B2652` | `004B1A52` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b26c4` | `TBD` | manual review - section 9 |
| `004B278F` | `004B1B8F` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b281e` | `TBD` | manual review - section 9 |
| `004B2912` | `004B1D12` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b29ee` | `TBD` | manual review - section 9 |
| `004B2B82` | `004B1F82` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b2c21` | `TBD` | manual review - section 9 |
| `004B58C7` | `004B4CC7` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x4b5939` | `TBD` | manual review - section 9 |
| `004B63CE` | `004B57CE` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4b63ee` | `TBD` | manual review - section 9 |
| `004D0731` | `004CFB31` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jae 0x4d07f7` | `TBD` | manual review - section 9 |
| `004D1471` | `004D0871` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jae 0x4d1535` | `TBD` | manual review - section 9 |
| `004DE114` | `004DD514` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jg 0x4de175` | `TBD` | manual review - section 9 |
| `004DF23A` | `004DE63A` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jg 0x4df2b7` | `TBD` | manual review - section 9 |
| `004E71CA` | `004E65CA` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jle 0x4e71f8` | `TBD` | manual review - section 9 |
| `004EA39C` | `004E979C` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x4ea3f9` | `TBD` | manual review - section 9 |
| `004EBC30` | `004EB030` | `81 7f 18 30 75 00 00` | `cmp dword ptr [edi + 0x18], 0x7530` | `jle 0x4ebc40` | `TBD` | manual review - section 9 |
| `004EC716` | `004EBB16` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jle 0x4ec724` | `TBD` | manual review - section 9 |
| `004F4B79` | `004F3F79` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x4f4bae` | `TBD` | manual review - section 9 |
| `004F4BFF` | `004F3FFF` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x4f4c34` | `TBD` | manual review - section 9 |
| `004F4D06` | `004F4106` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x4f4d3b` | `TBD` | manual review - section 9 |
| `004F5918` | `004F4D18` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x4f59bd` | `TBD` | manual review - section 9 |
| `005068BE` | `00505CBE` | `81 ff 30 75 00 00` | `cmp edi, 0x7530` | `jge 0x5069df` | `TBD` | manual review - section 9 |
| `00517E23` | `00517223` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x517ef4` | `TBD` | manual review - section 9 |
| `0052D3DE` | `0052C7DE` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x52d40b` | `TBD` | manual review - section 9 |
| `00537444` | `00536844` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x53748a` | `TBD` | manual review - section 9 |
| `00539679` | `00538A79` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jae 0x5398c6` | `TBD` | manual review - section 9 |
| `0057197F` | `00570D7F` | `81 ff 30 75 00 00` | `cmp edi, 0x7530` | `jge 0x571a28` | `TBD` | manual review - section 9 |
| `005861BB` | `005855BB` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x5861c7` | `TBD` | manual review - section 9 |
| `00588DA6` | `005881A6` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x588db2` | `TBD` | manual review - section 9 |
| `005A154F` | `005A094F` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x5a15b0` | `TBD` | manual review - section 9 |
| `005A1A58` | `005A0E58` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jle 0x5a1a74` | `TBD` | manual review - section 9 |
| `005A3BAD` | `005A2FAD` | `36 81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x5a3bc2` | `TBD` | manual review - section 9 |
| `005BF726` | `005BEB26` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jle 0x5bf760` | `TBD` | manual review - section 9 |
| `005C3BFE` | `005C2FFE` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jle 0x5c3c0f` | `TBD` | manual review - section 9 |
| `005C45EE` | `005C39EE` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jle 0x5c45ff` | `TBD` | manual review - section 9 |
| `005C4DB4` | `005C41B4` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jle 0x5c4dc5` | `TBD` | manual review - section 9 |
| `005CFDC3` | `005CF1C3` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jle 0x5cfdd4` | `TBD` | manual review - section 9 |
| `005CFFDF` | `005CF3DF` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jle 0x5cfff0` | `TBD` | manual review - section 9 |
| `005D5457` | `005D4857` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x5d556f` | `TBD` | manual review - section 9 |
| `005D6923` | `005D5D23` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x5d6b6b` | `TBD` | manual review - section 9 |
| `005D75E7` | `005D69E7` | `81 fb 30 75 00 00` | `cmp ebx, 0x7530` | `jl 0x5d75f3` | `TBD` | manual review - section 9 |
| `005D7815` | `005D6C15` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x5d7945` | `TBD` | manual review - section 9 |
| `005D7963` | `005D6D63` | `81 ff 30 75 00 00` | `cmp edi, 0x7530` | `jl 0x5d796f` | `TBD` | manual review - section 9 |
| `005D7B02` | `005D6F02` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x5d7b0e` | `TBD` | manual review - section 9 |
| `005D7C85` | `005D7085` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x5d7db5` | `TBD` | manual review - section 9 |
| `005D7DD3` | `005D71D3` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jl 0x5d7ddf` | `TBD` | manual review - section 9 |
| `005D7FF5` | `005D73F5` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jl 0x5d8001` | `TBD` | manual review - section 9 |
| `005D81F1` | `005D75F1` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x5d8299` | `TBD` | manual review - section 9 |
| `005D82C0` | `005D76C0` | `81 ff 30 75 00 00` | `cmp edi, 0x7530` | `jl 0x5d82d9` | `TBD` | manual review - section 9 |
| `005DA6B0` | `005D9AB0` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `cmovg` | `TBD` | manual review - section 9 |
| `005DB4EC` | `005DA8EC` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x5db4f8` | `TBD` | manual review - section 9 |
| `005DB670` | `005DAA70` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x5db683` | `TBD` | manual review - section 9 |
| `005E123E` | `005E063E` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x5e13c8` | `TBD` | manual review - section 9 |
| `005E5FF8` | `005E53F8` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x5e6139` | `TBD` | manual review - section 9 |
| `005E6602` | `005E5A02` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jl 0x5e660d` | `TBD` | manual review - section 9 |
| `005E6717` | `005E5B17` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x5e67ab` | `TBD` | manual review - section 9 |
| `005E9877` | `005E8C77` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x5e9a91` | `TBD` | manual review - section 9 |
| `005F4D05` | `005F4105` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x5f4d40` | `TBD` | manual review - section 9 |
| `005F5B57` | `005F4F57` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x5f5bb6` | `TBD` | manual review - section 9 |
| `005F674F` | `005F5B4F` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x5f6bb2` | `TBD` | manual review - section 9 |
| `005F69FD` | `005F5DFD` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jg 0x5f6a69` | `TBD` | manual review - section 9 |
| `005F8DE2` | `005F81E2` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x5f8e44` | `TBD` | manual review - section 9 |
| `00605C15` | `00605015` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x605ea6` | `TBD` | manual review - section 9 |
| `00614FCE` | `006143CE` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x6150cf` | `TBD` | manual review - section 9 |
| `0061E726` | `0061DB26` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x61e83c` | `TBD` | manual review - section 9 |
| `0061F0A1` | `0061E4A1` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x61f164` | `TBD` | manual review - section 9 |
| `0061F973` | `0061ED73` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x61f990` | `TBD` | manual review - section 9 |
| `0061FD41` | `0061F141` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x61fdf9` | `TBD` | manual review - section 9 |
| `0061FE69` | `0061F269` | `81 fa 30 75 00 00` | `cmp edx, 0x7530` | `jge 0x620023` | `TBD` | manual review - section 9 |
| `00623554` | `00622954` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x6235b4` | `TBD` | manual review - section 9 |
| `00628C28` | `00628028` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x628f5a` | `TBD` | manual review - section 9 |
| `0062F1BE` | `0062E5BE` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x62f1cc` | `TBD` | manual review - section 9 |
| `006307DF` | `0062FBDF` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x63090a` | `TBD` | manual review - section 9 |
| `006352CE` | `006346CE` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x6352dc` | `TBD` | manual review - section 9 |
| `0063974E` | `00638B4E` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x6398f7` | `TBD` | manual review - section 9 |
| `00643460` | `00642860` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jl 0x643479` | `TBD` | manual review - section 9 |
| `006585E9` | `006579E9` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jl 0x658601` | `TBD` | manual review - section 9 |
| `00669789` | `00668B89` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x6697cb` | `TBD` | manual review - section 9 |
| `0066B9BB` | `0066ADBB` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x66baeb` | `TBD` | manual review - section 9 |
| `0066BBFC` | `0066AFFC` | `3d 30 75 00 00` | `cmp eax, 0x7530` | `jge 0x66bc8e` | `TBD` | manual review - section 9 |
| `006874ED` | `006868ED` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x687723` | `TBD` | manual review - section 9 |
| `006A7B8B` | `006A6F8B` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x6a7c36` | `TBD` | manual review - section 9 |
| `006A80B3` | `006A74B3` | `81 fb 30 75 00 00` | `cmp ebx, 0x7530` | `jge 0x6a815e` | `TBD` | manual review - section 9 |
| `006AA862` | `006A9C62` | `81 ff 30 75 00 00` | `cmp edi, 0x7530` | `jge 0x6aa986` | `TBD` | manual review - section 9 |
| `006AAA81` | `006A9E81` | `81 fe 30 75 00 00` | `cmp esi, 0x7530` | `jge 0x6aacc9` | `TBD` | manual review - section 9 |
| `006ADEFD` | `006AD2FD` | `81 f9 30 75 00 00` | `cmp ecx, 0x7530` | `jge 0x6ae0ec` | `TBD` | manual review - section 9 |

### imm16 word guards (sample of 1)

| RVA | file off | bytes | disasm |
|---|---|---|---|
| `003CBBC4` | `003CAFC4` | `66 81 ff 30 75` | `cmp di, 0x7530` |
(+ 0 more — all MAX_ITEM_UNIQUE-class; excluded.)

## 9. Manual-review rows (30 sites)

Rows this sweep could not resolve from bytes alone: cmps with no branch to
read, isolated `>= 8,500` sites where the IsNPC pass and the instance fail are
indistinguishable, and branches that land inside a neighbouring cmp (flag-reuse
trampolines), where no shared fail label picks a role. Each needs its branch
shape read before the fork build touches it — §6's floor and §7's approved
ceiling give the two candidate readings, and the jump target between them
settles it.

| RVA | file off | old bytes | disasm | jcc | new bytes | rule |
|---|---|---|---|---|---|---|
| `00005D13` | `00005113` | `81 f9 34 21 00 00` | `cmp ecx, 0x2134` | `jae 0x5d23` | `TBD` | manual review - section 9 |
| `00006AEE` | `00005EEE` | `81 f9 34 21 00 00` | `cmp ecx, 0x2134` | `jae 0x6afe` | `TBD` | manual review - section 9 |
| `00041BC5` | `00040FC5` | `81 f9 34 21 00 00` | `cmp ecx, 0x2134` | `jg 0x41eea` | `TBD` | manual review - section 9 |
| `000DF54B` | `000DE94B` | `81 f9 20 4e 00 00` | `cmp ecx, 0x4e20` | `mov` | `TBD` | manual review - section 9 |
| `000E7CD3` | `000E70D3` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0xe7cf7` | `TBD` | manual review - section 9 |
| `00125E1B` | `0012521B` | `81 ff 34 21 00 00` | `cmp edi, 0x2134` | `jge 0x125e2f` | `TBD` | manual review - section 9 |
| `00126B3B` | `00125F3B` | `81 fe 34 21 00 00` | `cmp esi, 0x2134` | `jge 0x126b4f` | `TBD` | manual review - section 9 |
| `002335E0` | `002329E0` | `81 ff 34 21 00 00` | `cmp edi, 0x2134` | `jge 0x2335f4` | `TBD` | manual review - section 9 |
| `00233C8D` | `0023308D` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x233ca5` | `TBD` | manual review - section 9 |
| `003D0884` | `003CFC84` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x3d0899` | `TBD` | manual review - section 9 |
| `003D726A` | `003D666A` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x3d727f` | `TBD` | manual review - section 9 |
| `003D795B` | `003D6D5B` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x3d7970` | `TBD` | manual review - section 9 |
| `003D7C3E` | `003D703E` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x3d7c53` | `TBD` | manual review - section 9 |
| `003EAAD8` | `003E9ED8` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x3eaaed` | `TBD` | manual review - section 9 |
| `003F10B9` | `003F04B9` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jge 0x3f10c5` | `TBD` | manual review - section 9 |
| `003FA11A` | `003F951A` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x3fa133` | `TBD` | manual review - section 9 |
| `00414007` | `00413407` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x41401c` | `TBD` | manual review - section 9 |
| `0042003B` | `0041F43B` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x420054` | `TBD` | manual review - section 9 |
| `0042A3B0` | `004297B0` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x42a3c5` | `TBD` | manual review - section 9 |
| `0042A3F0` | `004297F0` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x42a405` | `TBD` | manual review - section 9 |
| `0042FC21` | `0042F021` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x42fc3a` | `TBD` | manual review - section 9 |
| `00574A76` | `00573E76` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x574a96` | `TBD` | manual review - section 9 |
| `005866A8` | `00585AA8` | `3d 34 21 00 00` | `cmp eax, 0x2134` | `jl 0x5866c8` | `TBD` | manual review - section 9 |
| `0060DA68` | `0060CE68` | `81 fe 34 21 00 00` | `cmp esi, 0x2134` | `jge 0x60da7a` | `TBD` | manual review - section 9 |
| `00648CD0` | `006480D0` | `81 bd d4 eb ff ff 20 4e 00 00` | `cmp dword ptr [ebp - 0x142c], 0x4e20` | `mov` | `TBD` | manual review - section 9 |
| `00648F07` | `00648307` | `81 bd d4 eb ff ff 20 4e 00 00` | `cmp dword ptr [ebp - 0x142c], 0x4e20` | `mov` | `TBD` | manual review - section 9 |
| `00649012` | `00648412` | `81 bd d4 eb ff ff 20 4e 00 00` | `cmp dword ptr [ebp - 0x142c], 0x4e20` | `mov` | `TBD` | manual review - section 9 |
| `006B8A06` | `006B7E06` | `81 7f 30 20 4e 00 00` | `cmp dword ptr [edi + 0x30], 0x4e20` | `mov` | `TBD` | manual review - section 9 |
| `006B8E64` | `006B8264` | `81 7f 30 20 4e 00 00` | `cmp dword ptr [edi + 0x30], 0x4e20` | `mov` | `TBD` | manual review - section 9 |
| `006B92EC` | `006B86EC` | `81 7e 30 20 4e 00 00` | `cmp dword ptr [esi + 0x30], 0x4e20` | `mov` | `TBD` | manual review - section 9 |

## 10. Reconciliation with Gate 0 and verification checklist

- Gate 0 (design doc §2.3) counted 217 IsPC zero-check sites, 239 dword `20,000`
  guards, 182 word `0x7530` guards and 54 IsNPC pairs. This sweep decodes and
  clusters differently: **370** floor-`3,000` sites (every
  `cmp ?, 3000`, not only the zero-check-adjacent ones — IsPC *and* instance
  floor share the immediate), **456** ceiling-`20,000` sites,
  **40** IsNPC pairs feeding **52**
  floor rows, **33** instance pairs feeding
  **28** ceiling rows, **1**
  instruction-confirmed word `0x7530` (Gate 0's 182 were raw 16-bit hits that
  also hit data), and **30** manual. Gate 0's counts prove where
  the walls are; this inventory is the apply-list.
- Checklist before the fork build ships: (1) every `TBD` row resolved by reading
  its branch shape; (2) §7's approved Option A rows applied (`0x7530`/`0x752F`); (3) post-patch
  resweep of the new binary shows zero old-immediates in families A/B/C and an
  untouched `0x7530` count; (4) lab boot against a horizon-constants server: a
  PC at id 15,000 renders as a PC, an NPC at 25,000 as an NPC, nothing above
  20,000 renders as a PC; (5) server constants flip lands in the same release
  (design doc §6-P2) — a patched client against the interim server
  misclassifies every NPC id 8,500..=19,999 as a PC, so the two ship together.
