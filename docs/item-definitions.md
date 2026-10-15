# Item definitions & item stats — layout, loaders, active/dead files

How the Goonzu client stores item definitions and base stats in memory, which data
files feed them, and what the three shipped clients (`New Era Live`, `New_Era_Live_update`,
`GoonZuNewHope`) actually load. Ground truth: the shipped binaries decompiled with
REA + Ghidra, cross-checked against the legacy C++ engine source tree. Every claim is
tagged with a source `file:line` or a binary address — nothing is inferred.

Binary addresses below are Ghidra procedure entry points (`FUN_…`), verified by
decompiling the function and matching its body against the source.

## 1. Three layers of "item" in memory

### 1.1 `cltItemTypeInfo` — the type registry row (`cltItem.h:506-522`)

```
SI32 siUnique;              // = ITEMTYPE_* enum value, used directly as array index
SI64 siAtb;                 // ITEMATB_* bitmasks (weapon/armour/trade/… behaviour flags)
SI64 siAttach;              // ITEMATTACH_* bitmasks (which stat-union member it uses)
TCHAR szName[128];          // localized type name (from text resource, not a file)
TCHAR szTypeCode[128];      // "SWORD", "ARMOUR", …
```

Stored in `pclItemTypeInfo[MAX_ITEMTYPE_NUMBER]`, `MAX_ITEMTYPE_NUMBER = 46`
(`cltItem.h:93`). Every row is **hardcoded in source**: `cltItemManagerCommon::SetItemType()`
(`cltItem-SetItemData.Cpp:22-193`) calls `SetItemType(type, atb, attach, code, name)` with
literal bitmasks — no data file defines type attributes. Binary: base `FUN_00892ec0`
(= the no-arg registry body; the `ATIFACTCRYSTAL` typecode is referenced only there),
which calls the setter helper `FUN_008ab230` (307 B) ~46 times; setter source:
`Item-ManagerCommon.cpp:326-345` (`pclItemTypeInfo[itemtype] = new cltItemTypeInfo(...)`).

Runtime override path: the server can push a new type registry over the network —
`DoMsg_GAMEMSG_RESPONSE_SETITEMTYPEINFO` (`Client-Item.cpp:382`) rewrites
`pclItemTypeInfo` entries. That is the only "override" mechanism; there are no
hardcoded per-item stat overrides outside the data files.

### 1.2 `cltItemInfo` — the per-item definition (`cltItem.h:623-741`)

One row per item `Unique`, allocated by `AddItem` (`Item-ManagerCommon.cpp:550-590`)
at the first free slot of `pclItemInfo[MAX_ITEMINFO_NUMBER]`, `MAX_ITEMINFO_NUMBER = 4000`
(`ItemCommon.h:9`); `ItemRef[MAX_ITEM_UNIQUE]` (`MAX_ITEM_UNIQUE = 30000`, `cltItem.h:99`)
maps unique → ref.

Field list in declaration order (`cltItem.h:625-741`):

| Lines | Fields |
|---|---|
| 625-629 | `siType, siImageIndex, siSmallImageIndex, siWeight, siSoundType` |
| 631-635 | `siItemGrade, siItemInfoAtb` (CANUSE bitmask), `clSkillInfo`, `clCondition` |
| 637-643 | `siSetItemIndex, szArmCode[24], szName[MAX_ITEM_NAME_LENGTH=60]` (`cltItem.h:94`), `m_bDormancy` |
| 646-657 | `siNeedRank, siDecDur, siUpgradeSkillUnique/Type, siExplainUnique, siAdapt, bCanEquipSwitch` |
| 660-663 | **attack block**: `siAttackRange, siDefaultHitRate, siAttackType, dwAttackInterval` |
| 666-674 | `siHungry, siHorseFoodSwitch, bFishingBaitSwitch, siOrg1..4Penalty` |
| 677-682 | consumable: `siLife, siMana, siDiseaseUnique, szDiseaseName[128], szDiseaseType[128], siHealEft` |
| 685-686 | skillbook: `siSkillUnique, siSkillLevelLimit` |
| 691-695 | scroll: `siScrollAdaptAttach, siScrollFunction, siScrollAbility, siScrollDecDuration, siScrollMax` |
| 700-726 | `siMakeKind, BagUseItemType, siFishClassification, siTradeServerMinNum, bImmutableUseDate, m_bNewMarket` |
| 730-732 | `CrystalData[MAX_CAN_CRYSTALABILITY_NUM]` |
| 736-741 | `pszMaterialText, pszToolText, clProductionInfo, clItem` |

`clItem` (line 741) is the key: the **template instance** — a full `cltItem` whose stat
union holds the item's default per-instance stats. The ctor (`cltItem.cpp:49-188`)
`ZeroMemory`s, copies name (with the `"NONAMEITEM"` fallback), and `clItem.Set(pclItem)`
(`cltItem.cpp:101`).

Binary-verified attack-block offsets (base `New Era.exe`, MSVC x86 thiscall):
`cltItemInfo+0xd4 = siAttackRange`, `+0xd8 = siDefaultHitRate`, `+0xdc = siAttackType`,
`+0xe0 = dwAttackInterval` — proven by `AddSwordItem` (`FUN_0088fd60`) storing the
hardcoded values `range=1, type=2 (ATTACKTYPE_SWORD)` and the `AttackDelay` column at
exactly those offsets, matching declaration order. Ctor: `FUN_00893600`.

### 1.3 `cltItem` — the item instance (`ItemUnit.h:1281-1331`)

```
SI16 siUnique;  SI16 siItemNum;
UI08 uiDateYear, uiDateMonth, uiDateDay;  UI16 uiDateUseDay;
SI32 siMakerPersonID;
stElement Element;                 // UI08 siType + SI16 siPower (element type/power), ItemUnit.h:504-514
union { BYTE cData8[16];           // MAX_ITEM_UNION_BYTE = 16, ItemUnit.h:1188
        cltItemCommon / cltItemWeapon / cltItemArmour / cltItemNeck / cltItemShoes /
        cltItemRing / cltItemTool / cltItemBag / cltItemMantle / cltItemStatusArmour /
        cltItemCrystal / cltItemAvatar* … };
SI32 siSID;  SI16 siPalette;  cltCrystalItem cltItemCrytal[MAX_CRYSTAL_NUM];
```

Every union variant starts with the same 8 bytes (`ItemUnit.h:618-654`):
`cltItemCommonInfo` = `uiDurability:11, uiMaxDurability:11, uiRareInfo:3, uiGetItemReason:7`
then `cltItemOptionInfo` = `uiType1:6, uiPara1:10, uiType2:6, uiPara2:10`. Category fields
follow (`ItemUnit.h:656-1076`):

| Variant | Stat bits after the common 8 bytes | Source |
|---|---|---|
| `cltItemWeapon` | `uiMinDamage:11, uiMaxDamage:11, uiMag:10` (12 B total) | `ItemUnit.h:656-680` |
| `cltItemArmour` | `uiAC:12, uiLuck:12, uiSkillUp:6` | `ItemUnit.h:683-708` |
| `cltItemBag` | `uiAC:10` | `ItemUnit.h:712-734` |
| `cltItemMantle` | `uiAC:10, uiDex:6, uiVit:6, uiStr:6` | `ItemUnit.h:737-763` |
| `cltItemRing` | `uiStr:4, uiDex:4, uiVit:4, uiMag:4, uiLuk:4, uiHnd:4` | `ItemUnit.h:932-962` |
| `cltItemNeck` | plain bytes `uiSkillPointUp, uiMaxUsedate, uiMaxSkillPoint` (UI08 each) | `ItemUnit.h:965-990` |
| `cltItemShoes` | `uiAC:10, uiMoveSpeed:6` | `ItemUnit.h:992-1014` |
| `cltItemStatusArmour` | `uiSkillUp:6, uiReserve:2` + `uiStr/Dex/Vit/Mag/Hnd/Luk/Wis:8` | `ItemUnit.h:1036-1076` |
| `cltItemAvatarRing` | `uiAC:6, uiStr/Dex/Vit/Mag/Wis/Luk/Hnd:5` | `ItemUnit.h:891-930` |
| `cltItemAvatarNeck` | `uiUpgradeSkillUnique:8, uiSkillValue:8, uiMaxUsedate:8, uiMaxSkillPoint:8` | `ItemUnit.h:855-888` |

These packed 16 bytes are exactly the item-stat union on the wire (already modelled in
this repo's proto layer). Instance creation: each `Add*Item` parser builds a `cltItem`
from the parsed defaults, e.g. `cltItem clitem(unique, usedate, &element, &clItemWeapon)`
(`cltItem-SetItemData.Cpp:2028+`), and `AddItem` stores it as the template.

### 1.4 Requirements: `cltItemConition` (`cltItem.h:293-312`)

```
SI32 siLevel, siStr, siDex, siMag, siVit, siHnd;
SI32 siSkillKind, siSkillPoint;
bool bForMan, bForWoman;  SI16 siGrownUp, siFameLevel, siBaseSkillType;
```

Binary-verified layout (base `FUN_008977f0`, NewHope `FUN_14061d5f0`): `this` fields at
`[0]=level [1]=str [2]=dex [3]=mag [4]=vit [5]=hnd [6]=skillkind [7]=skillpoint`,
`bForMan` at byte offset 32; fail codes 1=OUTOFSKILL, 2=OUTOFLEVEL, 3=OUTOFSTR,
4=OUTOFDEX, 5=OUTOFMAG, 6=OUTOFVIT, 7=OUTOFHND, 8=FORWOMAN, 9=FORMAN.

## 2. File → parser → field pipeline

Entry point: `cltItemManagerCommon::SetItemData()` (`cltItem-SetItemData.Cpp:195`, locale
branches at :199 Korea, :345 Japan, :487 China, **:617 English**, :743 Taiwan, :813 USA,
:886 NHN-China, :946 Europe). Binary: base `FUN_00891cb0`, update `FUN_0089e700`,
NewHope `FUN_140615bb0`.

Each `Add*Item(TEXT("Item\\Data\\ItemType<X>E.txt"))` runs an `NDataLoader` with a
**positional** column schema (`NDataTypeInfo[]`: type + buffer per column — order is the
contract, the header row in the file is documentation only). The parser reads columns
into locals, then:

- **stats** → the category struct's `Init(...)`, e.g. weapons:
  `clItemWeapon.Init(durability, mindamage, maxdamage, mag)` + `element.siType = AttackE`,
  `element.siPower = AttackR` (`cltItem-SetItemData.Cpp:2028-2196`, `AddSwordItem`);
  armour: `cltItemArmour.Init(durability, ac, 0, incskillamount)` (:3275-3443);
  ring: `clItemRing.Init(durability, ac, str, dex, vit, mag, luk, hnd)` (:5732-5904);
  shoes: `clItemShoes.Init(durability, ac, movespeed)` (:4901-5053);
  neck: `clItemNeck.Init(durability, skillpoint, maxusedate, maxskillpoint)` + 
  `siUpgradeSkillUnique = ConvertTextToSkillUnique(szIncSkillUnique)` (:4598-4731).
- **requirements** → `cltItemConition(level, str, dex, mag, vit, hnd, skillkind,
  skillpoint, man, woman, grownup, fame)`; e.g. swords pass `(level, STR, SKILL_USESWORD1,
  MountSkillPoint)` — level + STR + a mount-skill requirement straight from columns.
- **craft** → `cltItemSkillInfo(skill, skilllevel, makepbt, incpbt, hnd, mag)` (`cltItem.h:393`).
- **overrides** (deliberate, in code, not data):
  - the file's `Name` column is discarded — the client resolves the display name from the
    text resource: `GetItemTxtFromMgr(unique)` inside each parser (e.g. `:2111`);
  - attack range/type are hardcoded per category, not read from the file (sword range 1 /
    type 2, bow type 7 — visible as immediate stores in the decompiles);
  - `CanUseType` string → `ITEMINFOATB_*` bitmask via `ItemInfoAtbParser`;
  - item grade, `MinNum`, `unionmarket` come from trailing columns.

Column schemas are per-category (verified from file headers, e.g. `ItemTypeSwordE.txt`:
`Unique … AttackDelay, Weight, Skill, MountSkillPoint, MakepPbt, IncPbt, Level,
Condition(Str), Hnd, Durability, Usedate, MinDamage, MaxDamage, productUnit, Material,
Tool, Explain, AttackE, AttackR, Grade, ATB, MinNum, unionmarket` — 30 columns;
`ItemTypeRingE.txt` adds `AddStr/AddDex/AddVit/AddInt/AddLuk/AddHnd` + `DecDur`;
`ItemTypeShoesE.txt` has `AC, MoveSpeed`; `ItemTypeNeckE.txt` has
`IncSkillUnique, MinSkillAmt, MaxSkillAmt, Fame Condition`).

### Equip gate

`CheckCondition` prepares the character side (skill total with two overrides),
`cltItemConition::Check` verifies the item side in a fixed 1..10 order. Called from the
drag-equip paths (`MouseOperation.cpp:1836,2160,2631,2891`), `NEquipWindow.cpp:643`,
and `Char-Common.cpp:3230`. Full specification — check order, fail codes, the two
skill-level overrides, fame, and where the character-level gates live: **§7**.

## 3. Active vs dead data files (English locale)

**Active — loaded by the item pipeline (46 calls in the English branch, binary-verified):**
`ItemTypeSwordE, SpearE, StaffE, AxeE, BowE, GunE, ArmourE, HelmetE, BeltE, NeckE,
ShoesE, HealE, FoodE, BookE, MineralE, AnimalE, TreeE, PaperE, ClothE, RingE, EtcE,
ScrollE, DressE, IronE, DishE, DollE, HatE, TicketE, BagE, SeaFoodE, FishingRodE,
MantleE, AvatarMantleE, AvatarDressE, AvatarHatE, AvatarNeckE, AvatarRingE, QuestE,
CrystalE, CompositionMaterialE, EventitemE, BoxE, PremiumE, SoulGuardE` (all
`Item\Data\*.txt`). Notable details:

- `SoulGuardE` is loaded **only in the English branch**, through the Premium loader with
  flag 0: `AddPremium("Item\Data\ItemTypeSoulGuardE.txt", …, 0x22, 0)` — this call exists
  in the binary (`FUN_0088add0` base) but **not** in the source tree's English branch
  (`cltItem-SetItemData.Cpp:617-742`); source/binary drift.
- `ItemTypeTreeE/ClothE/EtcE` share the generic `AddItem(filename, name, type)` loader
  (`FUN_00888c90` base).
- Auxiliary actives (each binary-verified by string reference from a non-CRC loader):
  `SetItemInfo.txt` (set items, `Item-ManagerCommon.cpp:290`; loader `FUN_008968c0`),
  `SetItemStepInfo.txt` (`cltSetItemAbilityManager::FileLoad`, `cltItem-SetItemData.Cpp:8464`;
  loader `FUN_00890b30`, 3 locale paths), `SkillBookInfo.txt` (`cltSkillBookManager::FileLoad`,
  `cltItem.cpp:812-818`; loader `FUN_008946e0`), `GachaRareItemListE.txt` (client gacha
  manager `FUN_0095f530` — locale switch, English is `case 8`), `CashItemBuyInfoE.txt`
  (`FUN_006364a0` — locale select, then plain `fopen(...,"rt")`, not `NDataLoader`),
  `Item\Data\CharInfo\chariteminfoE.txt` (kindinfo, `kindinfo-Manager.cpp:377`),
  `ItemImageList.txt` / `ItemImageBigList.txt` (`Item-ManagerClient.cpp:60,451`).

**Checksum-listed but never loaded:** `Item\Data\TradeRank.txt` — the string exists once
(base `0xb63d0c`) and is referenced only by the CRC file-check list
(`NCRCFileCheckManager::Init`, base `FUN_00756d40`); `SetItemData` does not reference it,
although the source's English branch contains `AddTradeServerRank("item\data\TradeRank.txt")`.
The shipped client only CRC-verifies it.

**Dead — present on disk, zero string matches in the shipped `New Era.exe`:** `ItemAbility.txt`,
`ItemAvility.txt`, `itemtooltip.txt`, `ItemTypeItemBoxE.txt`, `ItemTypeBookEoriginal.txt`,
`ItemEnchantEffect.txt` (both copies), `ItemTypeRePackingPremium.txt`. No loader references
them in either the source tree or the binary.

## 4. Cross-client comparison

### 4.1 `New Era Live` vs `New_Era_Live_update` (both `New Era.exe`, x86)

- **Same loader set**: both English branches contain the identical 46 loader calls, same
  order, same files — no new item categories, no new files on disk (`Item\` trees have the
  same file set; 23 files differ in content).
- **One schema change: `ItemTypeBowE.txt` gained a trailing `DmgPct` column** (31→32
  columns; values 8-17, a damage-percent stat). The update binary consumes it:
  `AddBowItem` (`FUN_0088ba70`, update; base `FUN_0087f090`) defines one extra integer
  schema entry and stores **one extra field** into the definition.
- **That is a struct change, not a data-only change**: the update inserted an `SI32` into
  `cltItemInfo` at offset **0xdc** (right after `siDefaultHitRate`). Proof — the bow
  attack block in the update stores `range@0xd4, hitrate@0xd8, NEW@0xdc, type@0xe0,
  interval@0xe4`, and `AddSwordItem` (update `FUN_0089c7a0`) stores the hardcoded
  `type=2` at `0xe0` and `AttackDelay` at `0xe4` — both shifted +4 vs base. Fields from
  `siAttackType` onward are at +4 offsets in the update build; only bows populate the new
  field (other loaders leave it 0 via the ctor's `ZeroMemory`).
- Data diffs besides the schema: rebalanced values across weapons/armour/etc. (e.g. sword
  Bronze: `MountSkillPoint 5→3`), `ItemTypeHatE.txt` `IncSkillUnique`
  `ALL_MAKESKILL_UNIQUE→ALL_ALLSKILL_UNIQUE`, and **4 new box items** (uniques
  20441-20444 appended to `ItemTypeBoxE.txt`).

### 4.2 `GoonZuNewHope\GoonZu.exe` (x64, CEF-era client)

- **Same architecture**: `SetItemData` = `FUN_140615bb0`, per-category `Add*Item` family
  (`FUN_1405fdcc0`…`FUN_140614a0`), `cltItemInfo` ctor `FUN_140617d20` (same 18-param
  signature), `cltItemConition::Check` `FUN_14061d5f0` (identical fail-code logic),
  `SkillBookInfo` loader `FUN_140619360`. Method call takes an explicit `this`
  (`param_1`) — MSVC x64.
- **Three new item categories**, all `.tsv`, loaded in the English branch:
  `ItemTypeSpecialScrollE.tsv` (`FUN_140612610`), `ItemTypeCardE.tsv` (`FUN_140604710`),
  `ItemTypeNatureE.tsv` (`FUN_14060d9f0`). `SoulGuardE` keeps the shared Premium loader
  but gets its **own type 0x2e** (vs 0x22 in the x86 clients).
- A name→enum resolver (`FUN_140696bb0`, 38 `ITEMTYPE_*` strings) maps type names from
  data files to enum values — the TSV tables reference types by name.
- Item data ships **inside `DataClient.pak`** (custom `GZPK` container; `Item\Data\` is
  empty on disk). The pak is mounted by `FUN_1400120f0`/`FUN_1400163d0` (14 refs to
  "DataClient.pak"); the `GZPK` magic itself is not in `GoonZu.exe` — the format parser
  lives in another module (launcher/DLL), not analysed here.
- Debug strings reference `ItemType*E.tsv` box validation in **Portuguese**
  (`"[BoxRewardItem] linha %d: ItemUnique %d NAO EXISTE nos ItemType*E.tsv …"`) — this
  build targets a Portuguese (BR) locale.

## 5. Verified function index

| Function | base `New Era Live\New Era.exe` | `New_Era_Live_update\New Era.exe` | `GoonZuNewHope\GoonZu.exe` (x64) |
|---|---|---|---|
| `SetItemData` (locale branch dispatcher) | `FUN_00891cb0` | `FUN_0089e700` | `FUN_140615bb0` |
| `SetItemType` (type registry body) | `FUN_00892ec0` | — | via `FUN_140696bb0` (name resolver) |
| `SetItemType(type,atb,attach,…)` setter | `FUN_008ab230` | — | — |
| `AddSwordItem` | `FUN_0088fd60` | `FUN_0089c7a0` | `FUN_140613d70` |
| `AddBowItem` | `FUN_0087f090` | `FUN_0088ba70` | `FUN_140603950` |
| `AddArmourItem` | `FUN_00879300` | `FUN_00885ce0` | `FUN_1405fe1b0` |
| `AddPremium` (also loads SoulGuardE) | `FUN_0088add0` | `FUN_00897810` | `FUN_14060ee20` |
| `AddItem` (generic: Tree/Cloth/Etc) | `FUN_00888c90` | `FUN_008956d0` | `FUN_14060c470` |
| `cltItemInfo::cltItemInfo` (ctor) | `FUN_00893600` | `FUN_008a0050` | `FUN_140617d20` |
| `cltItemConition::Check` | `FUN_008977f0` | `FUN_008a4350` | `FUN_14061d5f0` |
| `cltItemManagerCommon::CheckCondition` | `FUN_008a3e40` | `FUN_008b0b30` | `FUN_14062c4d0` |
| `cltSkillBookManager::FileLoad` | `FUN_008946e0` | — | `FUN_140619360` |
| CRC file-check list (TradeRank lives here) | `FUN_00756d40` | `FUN_00760ea0` | `FUN_140480510` |

Full English-branch loader address lists for all three binaries (all 44-49 `Add*Item`
callees) are captured in this analysis session's REA artifacts
(`%TEMP%\opencode\rea-work\func-setitemdata-*.json` pseudocodes).

## 6. Notes for the Rust port

- `pclItemInfo` is a dense-by-ref array (ref = allocation order, unique→ref via
  `ItemRef[]`); definitions are immutable after load except for the runtime price fields
  and the server-pushed type registry.
- The per-instance 16-byte stat union is the same layout the wire protocol uses — the
  proto union already modelled in this repo matches `cltItem`'s union bit-for-bit
  (weapon 11/11/10, armour 12/12/6, ring 4×6, shoes 10/6…).
- Requirement checks are pure functions of `cltItemConition` + character stats + skill
  totals; the only non-obvious behaviour is the two `CheckCondition` overrides (fishing
  quest → skill 10, summon → 100) — see §7 before implementing the gate.

## 7. Equip Gate & CheckCondition Specification

How the client decides whether a character may equip an item: `CheckCondition` prepares
the character side, `cltItemConition::Check` verifies the item side. Both decompiled in
all three clients and matched line-for-line against the source — nothing below is
inferred.

| Function | Source | base `New Era.exe` | `New_Era_Live_update` | `GoonZuNewHope` (x64) |
|---|---|---|---|---|
| `cltItemManagerCommon::CheckCondition` | `Item-ManagerCommon.cpp:977-1045` | `FUN_008a3e40` | `FUN_008b0b30` | `FUN_14062c4d0` |
| `cltItemConition::Check` | `Item-Condition.cpp:27-264` | `FUN_008977f0` | `FUN_008a4350` | `FUN_14061d5f0` |

(The update build's `FUN_008a44b0`, previously listed next to `FUN_008a4350`, is **not**
`Check`: its decompile contains no fail-code stores.)

### 7.1 Check order and fail codes

`Check` verifies strictly in this order and returns at the first failure. Order proven
from the fail-code stores in all three decompiles; source `Item-Condition.cpp`:

| # | Check (source lines) | Fails iff | `ITEMEQUIP_FAIL_*` (`ItemUnit.h`) |
|---|---|---|---|
| 1 | skill (:30-37) | `siSkillKind != 0 && skillpoint < siSkillPoint` | `OUTOFSKILL = 1` (:471) |
| 2 | level (:39-46) | `siLevel != 0 && level < siLevel` | `OUTOFLEVEL = 2` (:472) |
| 3 | str (:47-54) | `siStr != 0 && str < siStr` | `OUTOFSTR = 3` (:473) |
| 4 | dex (:55-62) | `siDex != 0 && dex < siDex` | `OUTOFDEX = 4` (:474) |
| 5 | mag (:63-70) | `siMag != 0 && mag < siMag` | `OUTOFMAG = 5` (:475) |
| 6 | vit (:71-78) | `siVit != 0 && vit < siVit` | `OUTOFVIT = 6` (:476) |
| 7 | hnd (:79-86) | `siHnd != 0 && hnd < siHnd` | `OUTOFHND = 7` (:477) |
| 8 | male-only item (:88-95) | `kind ∈ {hash("KIND_HERO1"), 3, 5, 7} && bForMan == false` | `FORWOMAN = 8` (:478) |
| 9 | female-only item (:98-105) | `kind ∈ {2, 4, 6, 8} && bForWoman == false` | `FORMAN = 9` (:479) |
| 10 | fame (:254-262) | `siFameLevel != 0 && famelevel < siFameLevel` | `OUTOFFAME = 10` (:480) |
| — | otherwise (:264) | — | returns true |

Binary details (identical in base/update/NewHope): `bForMan` at
`cltItemConition+0x20`, `bForWoman` at `+0x21`, fame is an SI16 at `+36` compared
against `param_10`/`param_11` (the `famelevel` argument) — base `FUN_008977f0`,
update `FUN_008a4350` lines 42-56, NewHope `FUN_14061d5f0` lines 41-53. The hero/heroine
kind sets are numeric literals in every decompile (`3,5,7` male, `2,4,6,8` female) with
only `KIND_HERO1` resolved through the hash (`FUN_00756c50` base).

Codes 11-16 (`NOSUMMONHERO … WRONGEQUIPTYPE`, `ItemUnit.h:481-486`) belong to the summon
equip path; note that the `OUTOFSUMMONLEVEL = 12` emission exists only in commented-out
code (`Item-ManagerCommon.cpp:1071`, `:1139`).

### 7.2 The two skill-level overrides (`CheckCondition`)

`totalskilllevel` is what `Check` receives as its `skillpoint` argument (check #1) — the
overrides relax **skill** requirements only; level, stats and fame are never touched:

1. **Fishing quest → 10** (`Item-ManagerCommon.cpp:988-997`): if the item's
   `siSkillKind` total via `GetTotalSkillLevel` (:983-986) is `< 10` **and**
   `IsQuestGoing(QUEST_CLEAR_COND_FISHNOSKILL)` is true, the effective skill level
   becomes 10. The quest-clear-cond enum is value **161** — every binary passes the
   literal `0xa1`: base `FUN_00903400(0xa1,…)`, update `FUN_0090ffd0(0xa1,…)`
   (`FUN_008b0b30` line 64 → `local_14 = 10`), NewHope `FUN_1406a79a0(…,0xa1,…)`
   (`FUN_14062c4d0` line 100 → `iVar8 = 10`). All three guard it with
   `if (9 < totalskilllevel) skip` (base `goto LAB_008a3f67`, update line 62,
   NewHope line 95) — the quest only matters while the real total is below 10.
2. **Summon identity → 100** (`:999-1003`): if `clIdentity.siIdentity ==
   IDENTITY_PERSON_SUMMON` (`CommonHeader.h:478`, value **4**), the effective skill
   level becomes 100 — unconditional, evaluated after override 1, so it overwrites it.
   Binary: identity byte at `char+0xc8` (base `*(char*)(param_2+200)`,
   update line 70) / `char+0xfc` (NewHope line 110) `== '\x04'` → `100`.

After the overrides, `CheckCondition` picks the `hnd` argument as `GetLuk()` or
`GetHnd()` per country switch `0x9d` (`Switch_ManufactureInfluenceLuck`, source
`:1006-1014`; base `FUN_00757cf0(0x9d)`, update `FUN_00761e50(0x9d)` line 73,
NewHope `FUN_140481a20(0x9d)` line 115). If `bSetTotalBA` is false (byte at
`char+0x400`, base-verified; source `:1017-1032`), str/dex/mag/vit/hnd are passed as
`MAX_SI32` so the stat checks auto-pass — while **level, skill and fame stay the real
values** (`GetLevel()` is passed in both branches, `:1023` and `:1036`).

### 7.3 Fame requirement (necklace-only column 11)

- **Item side**: `cltItemConition.siFameLevel`, populated **only by necklaces** —
  positional **column 11** of `ItemTypeNeckE.txt`, header name `Fame Condition`
  (verified: col 11 of 27), read as `NDATA_INT32, &siFameLevelCondition`
  (`cltItem-SetItemData.Cpp:4669`; file loaded at `:639`) and passed as the ctor's 12th
  argument: `cltItemConition(0, 0, 0, 0, 0, 0, 0, 0, true, true, 0, siFameLevelCondition)`
  (`:4714`) — all other neck requirements are 0.
- **Character side**: `clIP.siFameLevel`, the character's **fame level** — same unit as
  the requirement; assigned at `CommonHeader.h:3016`, advanced by the DB delta
  `siIncFameLevel` (`DBMsgImportantParameta.cpp:19,42`), milestone-checked at
  10/20/30/40 (`char.cpp:6545-6560`). Passed at `Item-ManagerCommon.cpp:1030` and
  `:1043`.
- **Formula**: fail **iff** `item_req != 0 && char_fame_level < item_req` → fail 10
  (`Item-Condition.cpp:254-262`). The tooltip mirrors the same comparison, rendering the
  requirement red when unmet (`Item-Condition.cpp:546-558`).

### 7.4 Where the character-level gates actually live

- **Equip level requirements** come only from the txt `Level`/`Condition` columns →
  `cltItemConition.siLevel`, compared against `clIP.GetLevel()` (check #2); the
  `bSetTotalBA` guard never bypasses the level check.
- **Fishing rods carry no level (or any) requirement**: `AddFishingRodItem` builds
  `cltItemConition(0, 0, 0, 0, 0, 0, 0, 0, true, true, 0, 0)`
  (`cltItem-SetItemData.Cpp:6781`); the rod's `Skill`/`MountSkillPoint` columns feed
  **crafting** info instead (`clinfo(SKILL_MAKETOOL1, skilllevel, …)`, `:6778`), and
  `CanEquip` becomes the `bCanEquipSwitch` flag (`:6797`). Fishing is gated by
  **SKILL_FISH skill level**, never character level: `CharServer-Fish.cpp:94-102`
  (skill < 1 without the quest → refuse) with the same quest→10 override at `:148-151`,
  mirrored in `NFishingDlg.cpp:449-460`.
- **The one character-level gate in the fishing flow is the quest start condition**:
  `Quest\huntingGround06\4540E.txt` → `InitCondition / Level = 47` (Skill 0) —
  identical in base and update builds (this build requires **47, not 30**). Enforced
  against `clIP.GetLevel()`: client `NNewQuestDlg.cpp:663-669`, `NQuestDlg.cpp:242-248`,
  server `Server-Person.cpp:1864-1866`. Struct `cltQuestInitConditionInfo`
  (`Quest.h:714-731`), parsed at `Quest-LoadTxt.cpp:1050`. NewHope's quest 4540 is
  pak-resident and was not checked.
- **Summon level 20 is a hardcoded character-level constant at the market/hatch UI**,
  not an item condition: `#define LIMIT_LEVEL_HATCH_DOLL 20` (`Char-Common.h:488`),
  checked in `NSummonMarketBuy.cpp:296` and `NSummonHeroMarketBuy.cpp:251` before the
  buy order is sent. Doll items carry an all-zero condition
  (`cltItem-SetItemData.Cpp:1427`); the `LimitLevel` column of `ItemTypeDollE.txt`
  (values 0/20, the 20s on hero dolls) is **not read** by the source `AddDollItem`
  (14 schema entries, no variable for it — binary handler not decompiled).
- Summon equip (`CheckConditionForSummon`, `Item-ManagerCommon.cpp:1093+`) requires
  `SUMMON_STATUS_TAKEOUT` (`:1100`); its min-level check is commented out
  (`:1135-1142`, would emit `OUTOFSUMMONLEVEL`), as is the older whole-function variant
  (`:1047-1091`: `limitlevel = 100`, skill forced to 100, fame forced to 100).
