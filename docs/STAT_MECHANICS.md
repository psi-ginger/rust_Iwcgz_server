# GoonZu Stat Mechanics — Legacy Reference

**Purpose**: the design input for Cat's custom stat system (directive, 2026-10-20) —
exactly what every character attribute does in the legacy game, in formulas.

**Source tree**: `GoonZu Source Code\Server_Core\GoonZu_Source\GoonZuWorld\` (ANSI/CP949 —
ripgrep finds the ASCII identifiers; Korean comments and some files need
`Select-String -Encoding Default`).

**Method**: two structured research passes over the legacy tree (offense; survivability +
allocation), every formula transcribed from whole-function / whole-`if`-chain reads —
branch order and integer truncation preserved, never from a single grep line. Lines marked
★ were then re-read directly against the source file while compiling this document
(~35 spot-checks; both passes scored 100 % on every citation checked).

**Port status legend**: ✅ implemented in `goonzu-rs` today · ⬜ not ported ·
⚠ = port diverges from the shipped legacy (see §9/§10).

---

## 0. Two parallel stat systems (read this first)

Almost every formula exists **twice**, selected at runtime by
`pclClient->IsCountrySwitch(Switch_HealthAddChange)` — verified live in three independent
places (`Parametabox.cpp:7272`/`:7291` ★, `:7024` ★, `Server-Attack.cpp:930` ★):

| | Switch **ON** (new path) | Switch **OFF** (old path) |
|---|---|---|
| entry | `Set*` family called from `Parametabox.cpp:4531` | inline code in `Action` (`:4616-6944`) |
| damage vars | `siTotalPHysicalMin/MaxAttackDmg` | `TotalMinDamage/TotalMaxDamage` |
| STR term | `str*((interval*10)+(level*50))` divisors (§8) | `str/5` PvE, `str*2/5` PvP (`Common.cpp:3039-3050`) |
| defense | `siPHysicalDefensive` → basis-10000 `GetDefensiveRate` | raw `TotalAC` → `*1.3` reduction (`Server-Attack.cpp:971-991` ★) |
| hit/dodge | basis-10000 ranks (§2) | percent `AR/DR` (§2) |
| attack roll | `rand()%10000` window (`:593` ★) | `rand()%100` (`:637` ★) |

The **port assumes the ON path** for HP/MP formulas (§4) and for the hit-rank model in
this document, but its damage reduction `reduced_by_ac` mirrors the **OFF-path** arm
(`Server-Attack.cpp:968-991` ★ — its doc comment cites exactly that range). Which switch
the shipped `New Era Live` server runs was **not** verified from a live capture; it is a
country/config value not resolvable from source (§10.1). This asymmetry is design input
for the P3 equip pass — see §9.

---

## 1. The seven abilities

`siStr siDex siVit siMag siHnd siLuk siWis` (spend order: STR DEX MAG VIT HND LUK WIS),
plus `bonus` — unspent level-up points (`parametabox.h:95-151`).

- **Creation**: every character starts all-5 / level 1 / bonus 5
  (`parametabox.h:136-151`, `Server-Login.cpp:268`); under the switch, HND/WIS/LUK are
  pinned to 5 and life/mana seeded with the switch-ON formulas (`Server-Login.cpp:271-285`).
- **The allocation invariant** (the `level*5+35` DDL check), literally ★
  `Server-Person.cpp:2464-2476`:

```cpp
SI32 data = GetBonus() + GetStr()+GetDex()+GetMag()+GetHnd()+GetVit()+GetLuk()+GetWis();
SI32 siBaseValue = 40;
if(data != (siBaseValue + (GetLevel()-1) * 5)) { ... CloseSocket(); return; }  // kick
```

  `40 + (level-1)*5` ≡ `level*5 + 35` — the exact invariant the port's DDL and
  `allocate_stat` enforce (`stats.rs`); mismatch = immediate disconnect.
- **Every formula consumes `clTotalBA` totals**, not raw base: the sum of **14 layers** —
  base IP + equip + horse + guild + health-add + set-item + skill-add + transform + buf +
  elixir + hinamatsuri + soulguard + christmas-tree + valentine
  (`Parametabox.cpp:4327-4438`, per-stat pattern identical). Equipment and buffs enter the
  same formulas as base ability points.
- **Battle power** (EXP-gate input only, not damage): `GetBattlePower =
  (str+dex+mag+vit)/2` (`Parametabox.cpp:4162-4167`) — HND/LUK/WIS excluded; consumers
  `Server-Attack.cpp:3991, 5085, 7952`. Ported (P1) in `gz-data::kindinfo::battle_power`.

---

## 2. Hit, dodge, defense — the finals

### Attacker rank (switch ON)

```cpp
// physical — HND, ★ :2141
tempHitRateRank = ((totalHnd * 3) + level) + siAddPHysicalHitRate;      // 0 unless hnd>0 && level>0
// scaled by equip crystal + set-item + weapon-skill % (:2150):
rank + rank * (siEquipPHysicalHitRate + siAddPHysicalHitRateSetItem + siAddWeaponSkillHitRate) / 100

// magical — WIS, ★ :2262, scaled the same way ★ :2267-2273
tempHitRateRank = ((totalWis * 2) + level);
```

### Evasion rank (switch ON)

```cpp
// physical — DEX ★ :3199      magical — DEX :3343
temDodgerateRank = ((totalDex * 3) + level);      // ((totalDex * 2) + level)
// + crystal % :3215, weapon-skill % :3256, master-skill :3270, summed :3278
```

### Final hit / dodge values (switch ON) — basis 10000, level term `(mine − theirs) * 50`

```cpp
// GetHitRate, ★ :7060/:7066 (magical twin :7040), capped at 10000 = 100 %
siPHysicalHitRateLevel = (Mylevel - Enemylevel) * 50;
toTalHitRate = min(10000, ((7000 + (siPHysicalHitRateRank*6)) * (10000 + siPHysicalHitRateLevel)) / 10000);

// GetDodgeRate, ★ :7194/:7199 (magical uses rank*7, :7174), capped at 5000 = 50 %
toTalDodgeRate = min(5000, (siPHysicalDodgeRateRank*6*(10000 + siPHysicalDodgeRateLevel)) / 10000);
```

A defender's `CURSE_DODGE` subtracts from hit (`:7075`); `CURSE_HIT` adds to dodge
(`:7208`) — curses not ported. Client *displays* base 7500 (`GetClientHitRate :7129`)
while the server rolls with 7000 — display-only mismatch by design.

### The resolution — `HitOperation`, `Server-Attack.cpp:591-634` ★

```cpp
SI16 randHit = rand() % 10000;                                                   // :593
// DODGE:  hit < randHit <= hit + dodge/2
if ( siHitRate < randHit && randHit <= siHitRate + (siDodgeRate/2) )  { dodge msg; return FALSE; }  // :608
// MISS:   randHit > hit + dodge/2
else if ( siHitRate + (siDodgeRate/2) < randHit )                     { miss  msg; return FALSE; }  // :617
// else: lands
```

Where `hitrate` comes from the **attacker's** `GetHitRate(enemy, …)` and `dodgerate` from
the **defender's** `GetDodgeRate(attacker, …)`, packed into `cltAttackTarget` at dispatch
(`:117-133`). Net effect: success window `[0, hit]`; dodge only widens the *failure* band
by `dodge/2` — so dodge matters half as much as its number suggests, and a hit cap of
10000 with dodge ≥ 1 still never guarantees a hit (`dodge/2 > 0` band above `hit` —
capped hit 10000 + dodge band can exceed the roll space, making 10000 hit exact-cap
always land). Magic: `rand()%10000 > hitrate` → fail (`:3257`, `:3421` ★).

Old path (`:637` ★): `rand()%100 > siHitRate` → miss, dodge folded into hit% via `AR`/`DR`
(§3c).

### Defense — two arms, selected at `Server-Attack.cpp:930` ★

```cpp
if (IsCountrySwitch(Switch_HealthAddChange)) {                      // shipped-family path
    defensiveRate = defender.GetDefensiveRate(attacker[, magic]);    // :938-944
    validpower = siDamage - (siDamage * defensiveRate) / 10000;      // :960   basis 10000
} else {                                                             // old path
    ac = attackerTarget.GetTotalAC();
    decpower = ((siDamage * ac) / ((siDamage*2) + (ac*8/10))) * 1.3; // :985 ★  int div, double *1.3
    validpower = siDamage - decpower;                                // :988
}
if (validpower <= 0) validpower = 1;                                 // :991 ★
validpower = max(1, validpower - validpower * siEquipAssistDamageReduceRate / 1000);  // :996-999
```

`GetDefensiveRate` (both flags — magic `:6960`, physical `:6973`, both ★):

```cpp
rate = (((defensive*10000) / (defensive+700)) * (10000 + ((My-En)*200))) / 10000;
// clamped: max(1000, …) then min(9000, …)  -> 10 %..90 % band
```

Physical `defensive` base assembly (`SetPhysicalDefensiveRate :2502-2525`) =
**`(HND+level)*60/100`** (★ `:2322`) + armor crystal defense + kind default AC + transform
(+80+sync) + summon stone + marriage + horse WhiteTiger +100, then etc/cookie % — **VIT
and DEX contribute nothing to the new-path defense**; level only via the `(HND+level)`
term and the level-diff inside `GetDefensiveRate`.

Port note: `combat.rs::reduced_by_ac` is verbatim the **old-path** arm (cites
`:968-991`), fed with `level_ac(level)` (the `CalcLevelAC` ladder) because nothing else
is modeled (§9).

---

## 3. Physical attack, critical, interval

### 3a. The roll — `CalcApower` (`Parametabox.cpp:408-428`)

`min + GetFrame() % (diff + 1)` where `diff = max - min` (clamped ≥ 0) — **frame counter,
not `rand()`**, all integer. Called once per swing (`Char-Common.cpp:4884`,
`char-attack.cpp:2113`). Port: `combat.rs::roll_power` ✅ same expression.

### 3b. Assembly (switch ON) — `SetPhysicalAttackDemage` (`Parametabox.cpp:452-1386`)

1. **STR term** — the `ATTACKTYPE_*` switch of §8 (★ `:490-557`).
2. **Weapon base** — `GetWeaponMinDamage`/`GetWeaponMaxDamage` over the active slot
   (`:577-589`); unarmed/summon uses the kind's default weapon min/max (`:460-483`).
3. **Transform bonus** (`:601-626`): sword/spear +20/+20 at 100 % sync, axe
   `sync/2` both, gun `sync*0.4`.
4. **Summon wear-stone** (`:634-640`), **mount +100** (`:651-661`), **marriage** (`:666-690`).
5. **Flat sum** ★ `:695-696`:

```cpp
minDmg = defaultweaponmindamage + siEquipPHysicalMinAttackDmg + (SI16)siStrPHysicalAttackDmg
       + TminDmg + siAttackWearStoneMinDmg + siFireHatchMinDmg + siMarriageMinDmg;
// same shape for maxDmg — the STR term is cast to SI16 here (16-bit saturation)
```

6. **Percent pot** — one additive `CrysTalMinPer/CrysTalMaxPer` bucket: general-damage %
   (`:795`), combat-service +30 (`:804`), event buffs 10–30 (`:886-981`), set-item
   (`:1074-1096`), family cookie (`:1158`), naming quest (`:1221`), weapon-skill (`:1290`),
   master-skill (`:1328`).
7. **Finals** (`:1337-1362`): `total = base + base*pot/100` (int truncation after the
   multiply) → optional min/max **gap compression** via `siDecMinMaxGapRate` (`:1347-1351`)
   → clamp `min ≤ max` → curse % (`:1369`) and raccoon event +5 % (`:1375`).

### 3c. Old-path damage (switch OFF), `Parametabox.cpp:5948-5951`

```cpp
TotalMinDamage = TotalEquipMinDamage + siStrDamage + siLevelDamage + siTorpedoDamage + siEvolveDamage;
```

- `siStrDamage = str/5` PvE, `str*2/5` when `siEnablePVP==1` (`Common.cpp:3039-3050`) —
  no level, no interval term at all.
- `siLevelDamage = CalcLevelDamage(level)` ladder (`:56-89`, tiers 90/70/60/50/40 + `/10`)
  — ported as `combat.rs::level_damage` (the ladder the port currently uses for PC attacks).
- `AR = dex` exactly, `DR = AR/3 + TotalAC/2` ★ `:6779-6780` (+ weapon-skill % `:6805`);
  old-path hit% = `(AR*120)/(AR+enemyDR) + equip assist`, floor 60, cap 100 (`:7089-7102`).

### 3d. Critical

**Source** (`SetCritcalRate`, `Parametabox.cpp:3669-3900`) — **no ability stat feeds
crit**. Weapon-**skill** level keyed by attack type (SWORD→SKILL_USESWORD1 …
GUN→SKILL_USEGUN1, `:3678-3723`), then a ladder ★-pattern from the survivability pass:

```cpp
if(skilllevel > 30) { rate += (skilllevel-30)*2; skilllevel = 30; }   // :3726-3730
if(skilllevel > 20) { rate += (skilllevel-20)*5; skilllevel = 20; }   // :3731-3735
rate += skilllevel * 8;                                               // :3738
// + equip crystal flat :3744, weapon-skill % :3784, cap MAX_SCROLL_EFFECT :3788,
// transform axe sync/2 :3796; NPC: rate = pclKI->siCriticalRate (:3886, load at
// kindinfo-Manager.cpp:705 ★)
```

**Roll** — `Server-Attack.cpp:750-815` ★: rate ← `GetCriticalRate()` (+weapon 13600/13601
+5 % `:758`, curse `:764`, PC buf `:773`, set-item % `:784`), then

```cpp
if (siCriticalRate > (rand() % 1000))  Hit(attacker, target, &damage, 1);   // :801 ★
```

  → the roll is **per-1000 (permille)**: a stored rate of 10 = 1 % crit chance. This is
  the **only** crit roll in `Server-Attack.cpp` (verified by pattern search; `:3023/:3065/
  :3102` are unrelated `rate` procs).

**Application** — `GetDamage`, `:1806-1835` ★: `specialattackswitch == 1` (critical) →
`validpower *= 2` (`:1833`) — **×2 on the post-defense damage** (the commented-out
transform 2.2× axe variant is dead). `specialattackswitch` semantics (`:2181`): 0 normal,
1 critical, 2 watermelon extra hit (20 % roll `:807-814`, adds 79–94 % of damage
`:2204/:2372`, not ported). Port: `combat.rs::apply_critical` doubles on
`roll % 100 < rate` ⚠ — **per-100, so ~10× the legacy frequency for the same stored
rate** (§9.2).

### 3e. Attack interval

`dwAttackInterval` is a **pure data field** — weapon item row (`cltItem.h:663`
“공격과 다음 공격까지의 시간(ms)”, read via `GetWeaponAttackInterval`,
`Item-ManagerCommon.cpp:4366`) or kind table (`kindinfo-Manager.cpp:703` ★
`= Attack_Interval`); bare-handed falls back to `pclKI->dwAttackInterval`
(`Parametabox.cpp:4143`). **No level, DEX, STR or HND term anywhere.** Modifiers:
master-skill `SKILLMASTER_ATTACKINTERVAL` percent reduction (`:3857`); per-weapon-skill
reduction is commented out (`:3821-3841`); transform-axe at 100 % sync pins 1000 ms
(`:3804`). Enforced in `TryAttack` (`char-attack.cpp:1616-1632`): cooldown =
`interval − (interval/100 * buf%)` ms against `CurrentClock`.

**Design note**: `dwAttackInterval` is an operand of the STR term — **slower weapon ⇒
more STR damage**; that is the only speed↔power trade-off in the formulas. Port:
`attack_interval_ms` reads the weapon row only ✅ (structurally equivalent).

### 3f. Magic attack (MAG), for completeness

`SetMagicalAttackDemage` (`Parametabox.cpp:1389-2002`, cross-cited by both passes):

```cpp
minDmg = (totalMag * (4000 + (level*50)) / 20000);   // :1404
maxDmg = (totalMag * (6000 + (level*50)) / 20000);   // :1405
```

MAG + level only — no weapon base, no interval, no DEX; then equip magic %, mount +100,
general %, combat-service +30, buffs, finals with `SI16` cast and `min ≤ max` clamp
(`:1960-1977`). Consumers: `MagicMgr.cpp:1645-1646` (cast roll — its pipeline not read,
§10). Magic *hit* uses WIS, magic *defense* uses WIS — MAG does neither.

---

## 4. Max HP / Max MP

`GetMaxLife`/`GetMaxMana` (`Parametabox.cpp:7268-7282`/`:7287-7301`) ★ dispatch on the
switch: ON → `GetTotalHitpoint()`/`GetTotalManapoint()`, OFF → inline `MaxLife`/`MaxMana`.

**SetHitPoint (ON), `:3366-3520`** — base ★ `:3374` `siVitHitpoint = (totalVit * (8000 +
(level*5))) / 1000;` → equip crystal % (`:3394`) → weapon-skill VIT% (PC `:3412-3435`) →
magic buf (`:3440`) → marriage (`:3460`) → **×5** vs marked PVP enemy (`:3477` ★) /
PVP-league enemy (`:3481` ★) → ×5 enemy-guild hunt-map war (`:3489` ★) → set-item HP %
additive (`:3497-3501` ★) → event buf +10 % at buf ≥ 3 (`:3507` ★) → clamp current life to
total (`:3514-3520` ★).

> **Shipped-binary note (2026-10-21):** the deployed `New Era.exe` carries
> `SPEC_CLIENT_REBUILD_PATCH.md` §11, so at runtime `:3374`'s level term is
> **`level*50`** while the source tree above still reads `level*5`. The patch
> touches `SetHitPoint` only — `SetManaPoint` and every `×5`/`÷1000` divisor in
> this section are untouched, and `bnpc` never reaches the HP base term, so NPCs
> and monsters follow the same curve as PCs.

**SetManaPoint (ON), `:3537-3616`** — base ★ `:3545` `siMagManapoint = (totalMag * (4000 +
(level*5))) / 1000;` → equip % (`:3565`) → set-item % (`:3574`) → weapon-skill INCMAG %
(`:3582-3605`) → clamp (`:3612`). No PVP ×5 on mana.

**Switch-OFF parallel** (dead under ON, still compiled): `MaxLife = vit * VIT_LIFE_RATE`
(=4, `parametabox.h:11`), `MaxMana = mag * INT_MANA_RATE` (=4, `:12`), inline at
`:5766-5899`. Known drift bug: `:5857` scales the set-item **MP** % off **MaxLife** — do
not port.

**Monsters**: base abilities from `clKI->clBA` (`CommonLogic-SetChar.cpp:81`); switch-OFF
adds level scaling into raw stats — `str += (level-1)*2`, `dex/mag/vit += (level-1)` (mag
only if present) (`:113-124`) — then life/mana to max (`:146/:150`). The port takes
monster scaling from the kind .dat rows (`MonsterStats::at`) instead — data-driven rather
than formula-driven.

Port ✅: `derived_max_mana` = the ON base term ★ (verified formula, `stats.rs`).
`derived_max_life` is §4.3.4's deliberate `×50` reformation and tracks the
**patched** client rather than the legacy source — the two are pinned together
in `SPEC_CLIENT_REBUILD_PATCH.md` §11 (the same `×50` term also lives in
`kindinfo::MonsterStats::at`).

---

## 5. Regeneration and hunger — no stat participates

No HP/MP regen stat exists anywhere (grep `RegenHP|RegenMP|RecoverHP|RecoverMP|siHPRegen|
siMPRegen`: zero hits; real names `AutoRecoverLife/Mana`):

- 50-second timers in the char action loop (`char.cpp:2396-2472`); MP-recovery buf
  shortens the mana timer (`:2429-2438`).
- `AutoRecoverLife` (`:5255-5284`): `uiHealthPoint == 100` → no regen; `> 100` → the
  overshoot **is** the regen amount (headroom-clamped). Mana twin `:5335-5370` (buf floor 10).
- Food raises `uiHealthPoint` to max 110 (`Char-Item.cpp:3513`) — the source of overshoot regen.
- Sitting on a cushion: +40 (special uniques) else +20 every 5 s (`char.cpp:5299-5311`).
- **Hunger** (`MakeHunger`, `:6935-6955`): `max(1, level/8)`, ×5/2 over-weight, plus
  `addHunger` — the weak-monster block's side effect (`Server-Attack.cpp:4003-4056`: kills
  vs battle-power-inferior mobs add 5..1, killer level ≥ 20, only active at killer level ≥
  50). Hunger → health system → `uiHealthPoint` → regen magnitude.

Port ⬜ (P1 cut hunger/tiredness **EXP** effects; the weak-monster block including
`addHunger` sits behind `WEAK_MONSTER_PENALTY = false`, 2026-10-20 directive).

---

## 6. Level-up and the allocation hook

`LevelUpAction` (`char.cpp:5763-5801`) ★ — exactly the P1 port order:

```cpp
pclCI->clIP.IncreaseLevel(1);          // :5786  clamped MAX_CHARACTER_LEVEL
pclCI->clIP.IncreaseBonus(5);          // :5789  +5 unspent points
pclCI->clIP.IncreaseSkillBonus(1);     // :5792  +1 skill point
SetLife(clPB.GetMaxLife());            // :5795  full heal to new max
SetMana(clPB.GetMaxMana());            // :5798  full refill
SendDBLevelInfo(5, 1);                 // :5801  DB delta (+5, +1)
```

then `SetUpdateSwitch(UPDATE_IP, …)` (`:5974`) flushes the 215 `RESPONSE_PERSON_IP`
self-frame from `SendChangedDataToClient` (`Char-UpdateChangedData.cpp:74-83`). A dormancy
equip-reward hook runs first for PCs (`:5770-5783`).

**Thresholds & cap**: `IncreaseExp` (`Parametabox.cpp:43-53`) levels when
`curExp ≥ GetRequiredExpForLevel(level+1)`; table `LevelUpExp.dat` (`LoadExpData :8300`,
`GetRequiredExpForLevel :8226`); cap `MAX_CHARACTER_LEVEL = 120` (100 under `_NHN_CHINA`),
`CommonHeader.h:68-74`, clamped in `IncreaseLevel` (`parametabox.h:169-175`). Port ✅
`LevelUpTable`, verified row-for-row against the shipped .dat (threshold 75 at L2,
9,713,379,124 at L120).

**Spending — 250 `REQUEST_USEBONUS`** (`Server-Person.cpp:2449-2644`) ★:
invariant check (§1; mismatch = `CloseSocket`) → `bonus ≥ amount` → switch
**0=STR 1=DEX 2=MAG 3=VIT 4=HND 5=LUK 6=WIS** (`:2500-2540` ★), `Increase*` +
`DecreaseBonus`; HND blocked at `level*2` unless `Switch_CharStatusReform` (`:2519-2525`
★). Negative amount = reset via ticket `ITEMUNIQUE(24035)` (`:2555`), floor 5
(`:2544`). Apply → 215 flush + DB `sDBRequest_SetBonusInfo`. Port ✅
`stats.rs::allocate_stat` (invariant + HND cap tested).

**Skill points**: +1/level; skill-book learning requires `skill_bonus > 0`
(`Char-Common.cpp:3665-3678`), decremented via `GAMEMSG_REQUEST_DECREASESKILLBONUS`.
Weapon-skill EXP (P2) matters beyond EXP: weapon skill already feeds **crit rate, HP %,
MP %, hit/evasion %, and attack interval** in legacy.

**Derived refresh**: `LevelUpAction` itself only refills life/mana; the full param box
rebuilds on the next `ParametaBoxAction()` — 20 s server / 500 ms client
(`CharManager.cpp:90/:109`), or forced (`SetParametaBoxAction(true)`); every formula
reads `level` fresh, so the new level is baked in at that pass.

---

## 7. The class model — there is none (for PCs)

`class_kind|siClassKind|GetClassKind|CLASSKIND` — zero matches in the tree. PCs are
created identically for every class (all-5); `cltKindInfo` feeds **data**, not formulas:
default weapon min/max (`Parametabox.cpp:460-483`), default AC (`:2314`), move speed
(`:3907`), attack interval (`:4143`). Per-class formulas in the custom system = new
invention, not restoration.

---

## 8. Weapon-type branches — the STR term

`SetPhysicalAttackDemage`, ★ `Parametabox.cpp:490-557`, `switch(siAttackType)` with
`X = (dwAttackInterval*10) + (level*50)`:

| Cases | Term | Line |
|---|---|---|
| `SWORD` `SPEAR` `AXE` | `totalStr * X / 20000` | ★ `:495` |
| `BOW` `BOW1` `NEW_BOW1` `GUN` `GUN1` `NEW_GUN1` | `totalStr * X / 150000 * 6` | ★ `:503` |
| `STAFF` `STAFF1` `STAFF2` `NEW_STAFF2` | `totalStr * X / 150000 * 3` | ★ `:509` |
| ~30 cosmetic event/NPC types (ghost, cake, archer variants …) | `… / 150000 * 6` | ★ `:545` |
| `SIMTORYONG` `MAMARACCOON` `RACCOONTOWER` `GREATWALLSPIDER` | `… / 20000` | ★ `:551` |
| **`default` — anything unlisted incl. bare-handed** | `… / 20000` | ★ `:555` |

- **Bow = gun literally** — one case list, same divisor, **no DEX weight, no per-weapon
  denominator**. Staff is exactly half the ranged term.
- Truncation: C parses `((totalStr*X)/150000)*6` — integer division **once** at the
  divisor, then `*6`; a small STR × low interval rounds to **0** before the `*6` (e.g.
  X=12500, STR 1 → 12500/150000 = 0). Melee truncates once at `/20000`.
- **Speed⇄power**: interval sits inside `X` — slower weapon ⇒ more STR damage (§3e).
- Attack-type identity: `CalculateAttackType` (`:4059-4159`) — `GetWeaponAttackType`,
  staff falls back to melee when MP < 2 (`:4099-4106`).

Port ⚠ `combat.rs::str_damage`: melee + bow/gun ✅, but the `else` arm returns **0** for
`STAFF` *and* every unlisted type — legacy gives staff `/150000*3` and default
**`/20000`** (incl. bare-handed). See §9.1.

---

## 9. What `goonzu-rs` implements today — mapping and divergences

### Divergences found (decisions for Cat — no unilateral fixes; combat formulas are frozen)

1. **STR term fallback** (§8): port `else → 0` vs legacy staff `/150000*3` and
   default/bare-handed `/20000`. Reachable whenever `attack_type` is `STAFF*` or an
   unlisted type. Candidate fix belongs in the P3 weapon-damage batch.
2. **Critical denominator**: port `apply_critical` = `roll%100 < rate`
   (`monsters.rs:1260` passes `up_to(99)`); legacy roll = `rate > rand()%1000`
   (`Server-Attack.cpp:801` ★, the only crit roll in the file). Same stored kind value →
   port crits ≈ **10× as often**. (Kind .dat `criticalrate` magnitudes not re-checked —
   if the column held permille values the legacy intended, port would be 10× *correct*;
   either way the two disagree.)
3. **Damage-reduction arm** (§0/§2): port `reduced_by_ac` = the switch-**OFF** raw-AC
   `*1.3` arm with `level_ac` as the AC; the shipped-family path (if the switch is ON)
   reduces by basis-10000 `GetDefensiveRate` built from `(HND+level)*60/100` + armor.
   The formula *shape* and the AC *source* both differ. Natural resolution in the P3
   equip pass once HND/armor exist.

### Ability map

| Ability | Legacy effects | Port status |
|---|---|---|
| STR | phys-attack term (§8), weight `*200 + level*800` ★, battle power, old-path `/5` | damage ✅ (⚠ fallback), battle power ✅, weight ⬜ |
| DEX | phys/mag dodge ranks, old-path `AR`, battle power — **no damage term** | battle power ✅, dodge ⬜ |
| MAG | magic min/max, max MP ★, battle power — **not** magic hit/def | max MP ✅, battle power ✅, magic ⬜ |
| VIT | max HP ★ (plus HP ×5 PVP flags) — nothing else | max HP ✅, battle power ✅ |
| HND | phys hit rank ★, phys defense `(hnd+level)*60/100`, `level*2` cap ★, big-item weight 30 %, equip-wear/craft/market gates | allocation + cap ✅, effects ⬜ |
| WIS | mag hit rank ★, mag defense ★ — **not** max MP | allocation ✅, effects ⬜ |
| LUK | **no combat formula** — big-item drop roll (float, §1), region drop rolls, old-style luck weight 70 %, wear gates | allocation ✅, effects ⬜ |

### Mechanics present vs absent in the port

Present: level ladders (`level_damage`/`level_ac` — *old-path* constructs), STR term,
old-path AC reduction, frame-counter variance, crit doubling (⚠ denominator), per-hit EXP
+ level-ups (P1), weak-monster penalty **off** (`WEAK_MONSTER_PENALTY = false`),
table-driven attack interval.

Absent (the design space): hit/dodge ranks + the `rand()%10000` window, basis-10000
defense, magic combat, weapon-skill feeds (crit/HP/MP/interval/hit%), the 14-layer
`clTotalBA` totals (P3 starts equip), weight, regen/hunger/food, curses, luck rolls,
transform/mount/marriage/set-item/event stat layers.

---

## 10. Open questions / second-look list

1. **Which switch the shipped server runs** — `Switch_HealthAddChange` is a runtime
   country/config value; not resolvable from source. The port's HP/MP formulas assume ON
   (the `SetHitPoint`/`SetManaPoint` base terms are ON-only), while its damage reduction
   assumes OFF (§9.3). Reconcile once observable (e.g. a level-1 vit-5 char's live max HP:
   40 = ON, 20 = OFF).
2. **Wire ordinals vs enum labels**: this tree's `MsgType.h:252` holds
   `GAMEMSG_RESPONSE_PERSON_IP`; the port's wire values (215/270/7221) come from its own
   verified `opcodes_game` table — enum position ≠ wire id; don't trust `MsgType.h`
   ordinals.
3. **Magic-cast pipeline** (`MagicMgr.cpp:1645-1646` consumers of the magic totals) —
   the storage formulas are read; the per-spell roll/multiplier is not.
4. **`etcPhysicalDefensiveRate` (`:2548-2912`)** and the old-path post-multiplier block
   (`Parametabox.cpp:6000-6300`) — percent accumulators cited but not transcribed.
5. **Constants unresolved**: `MAX_SCROLL_EFFECT`, `VIT_LIFE_RATE`/`INT_MANA_RATE` taken
   as 4 from `parametabox.h:11-12` (report), `PERSON_SUMMON_ATTACKBONUS_PERCENT`.
6. **HND/WIS creation defaults** — `Init()` pins str/dex/vit/mag/luk = 5 only; hnd/wis
   from the base-class ctor (explicit 5s only under the switch, `Server-Login.cpp:273-274`).
7. **`uiHealthPoint` steady-state in combat** — food/cap (110) verified; decrement sites
   not enumerated. `siRegenerateLifeValue` default (server config) not traced.
8. **Big-item drop level gate**: `GiveBigItem` refuses beyond a 30-level gap
   (`Server-Attack.cpp:7548-7549` ★) — a level-difference rule the penalty-removal
   directive did not touch; confirm intended behaviour with Cat alongside the LUK design.
