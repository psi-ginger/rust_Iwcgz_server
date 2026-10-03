# T-014 · Combat formula simulator — findings

**Status:** proposal validated (numbers tested); no game code changed.
**Tool:** [`tools/sim`](README.md) (`sim.py`, self-test 48/48).
**Input:** real deployed data under `goonzu-server\deploy\GameServer`.
**Baseline tested:** [`combat-mechanics-audit-baseline.md`](../../../docs/rust-rewrite/reference/combat-mechanics-audit-baseline.md) §4 (status: proposal, untested).

Every number below is copied from `out/summary.txt` / `out/*.csv`, which
`sim.py` generated. Nothing here is typed from memory.

Reproduce:

```powershell
cd goonzu-rs\tools\sim
python sim.py            # runs the 48-check self-test, then writes out\
```

---

## Verdict

The audit's **problem diagnoses are sound**, and its worked examples reproduce
exactly. But the **proposed formulas do not yet cohere with each other or with
the §4.8 tower targets**: §4.6 under-mitigates by ~2.5×, the HP "same as before"
claim compares against the wrong legacy baseline, and the weight change is
framed as an upgrade while it is a high-level nerf. Details below.

---

## Findings

### F1 — §4.6 does not reproduce the §4.8 tower targets (quantified)

`tower_ttk.csv`. R8 = 850 raw, player L80, monster AC 550, HP 7500:

| path | per-hit | hits to kill |
|---|---|---|
| legacy Classic (hyperbolic AC) | 567 | 14 |
| legacy Renewal (`GetDefensiveRate`) | 476 | 16 |
| proposed §4.6 | 762 | 10 |
| §4.8 target | **500–650** | **7–10** |

The proposed per-hit (762) is **above** the §4.8 band (500–650). To land R8(850)
at 650 down to 500 per hit at L80, §4.6 needs **AC 1446 … 3290**, not the AC
450–650
§4.8 specifies. Either §4.6's constant/level term is too weak, or §4.8's AC
target is on a different scale. **These two sections cannot both be right.**

### F2 — §4.6's `Level` is unspecified and the choice swings the result 4×

§4.6 writes `DEF + 700 + (Level × 50)` without saying whose level. For a L100
player with DEF 771 (`mitigation_level_owner.csv`):

| reading | mitigation |
|---|---|
| `Level = 0` (ignored) | 52.41% |
| monster level 78 | 14.35% |
| player level 100 | 11.91% |

The legacy `GetDefensiveRate` uses *both* levels (defender − attacker) × 200, so
there is a precedent to copy; §4.6 drops it without saying what replaces it.
**This must be pinned before the formula can be implemented.**

### F3 — proposed mitigation is far weaker than legacy for the same DEF, and uncapped

`defense_curve.csv`, DEF = 1000 at level 100: legacy **58.82%** vs proposed
**14.92%**. The legacy formula floors at 10% and caps at 90%
(`defense_curve.csv` level 100, DEF 0 → 10.0, DEF 550 → 44.0). The proposed
formula has **no floor**: `DEF=0 → 0.0%`. Consequences:

* A naked/low-gear player resists **nothing** under the proposal, where legacy
  guaranteed 10%.
* Because the redesign `+Level×50` term grows, late-game mitigation collapses
  unless DEF grows much faster than the audit's own §4.1/§4.3 numbers
  (L100 full set = gear 94 + VIT-natural 677 = **771**, which the legacy formula
  turns into ~75% but the proposed formula turns into ~12%).

The audit's intent ("DEF scales up") requires a *much* larger DEF budget than
§4.1/§4.3 provide. **The DEF budget and the §4.6 divisor are not co-tuned.**

### F4 — the weight proposal is a high-level nerf, not an upgrade

`stat_curves.csv`, STR 550:

| level | legacy `STR*200 + Lv*800` | proposed | delta |
|---|---|---|---|
| 1 (STR 10) | 2800 | 5008 | +79% |
| 100 (STR 550) | 190000 | 123800 | **−34.8%** |

§4.3 item 8 presents the L100 value (123,800) as the upgrade but omits the legacy
190,000. It changes `Lv*800` → `Lv*8` and adds `3000 + 1000·⌊Lv/10⌋`, which is a
large **cut** at high level. Probably unintended; needs an owner decision.

### F5 — the "old HP" the proposal modifies is the renewal formula, not true legacy

§4.3 item 4 (line 196) states the old formula explicitly: `8.0 + 0.005*Lv`,
which is algebraically identical to the renewal `VIT*(8000 + Lv*5)/1000`
(`8.0 + 0.005*Lv ≡ (8000 + 5*Lv)/1000`). True legacy (`parametabox.h:11`) is
`VIT * 4`.

`stat_curves.csv`:

| formula | L1 (VIT 10) | L100 (VIT 550) |
|---|---|---|
| true legacy `Vit*4` | **40** | 2200 |
| audit "old" = renewal `8.0 + 0.005·Lv` | 80 | 4675 |
| proposed `8.0 + 0.05·Lv` | 80 | 7150 |

The audit's Lv.1 "เท่าของเดิมเป๊ะ" (exactly the same as before) is true against
*its own stated old formula* (8.005 → 8.05; both floor to 80 for VIT 10), but not
against true legacy — relative to `Vit*4`/`INT_MANA_RATE` the update **doubles**
level-1 HP. The proposal is therefore a change to the **renewal** generation;
whether that is the generation the rewrite replaces must be stated, because the
two baselines differ 2× at L1 and >3× at L100, and every later balance decision
inherits that choice.

### F6 — proposed elemental reduction is unconditional, so it also reduces non-elemental hits

`elemental.csv`. Legacy for `ElementA = 5`: grade 1 = **0**, grade 2 = **0**,
grade 3 = **0**, grade 6 = **1**, grade 10 = **2** (the integer-division bug).
Proposed: **6% per grade, regardless of monster ElementA**, so grade 10 = 60% on
*any* hit — including a monster with `ElementA = 0`. Legacy scaled with the
incoming element (and thus did nothing for non-elemental monsters). The
redesign needs `pct = f(grade) × f(ElementA)` (or a flat absorber), not an
unconditional percentage. The fix direction in the audit is right; the formula
as written is not.

### F7 — at high level, the proposed stat term overtakes the weapon

`swords.csv`. Proposed STR damage vs the weapon's own max damage:

| sword | level | weapon max | proposed STR term |
|---|---|---|---|
| Advanced Modular Sword | 150 | 830 | **1562** |
| 해왕의검 | 200 | 1347 | **2749** |

By ~L150 the stat term is ~1.9× the weapon's max, so weapon tier stops mattering
and the "weapon balancing spec" premise breaks. The proposal removed the `/20000`
but added no cap/soft-cap on the stat term. (The real L200 sword min 1089
matches the audit's stated 1,089 lower bound.)

### F8 — the proposed hit formula has a flat 70% base and cancels levels

`hit_dodge.csv`. With equal AGI and equal level, proposed effective hit is
**70.0%** at every level (because hit `7000 + rank*6` and dodge `rank*6` cancel,
and §4.7 removes the level term). AGI only shifts ±18 per stat-point; §4.7 does
fix the level-gap miss (legacy L100 vs L200 = 42.5% vs proposed 64.0%). But a
constant 70% base hit floor is a design choice §4.3 item 2 does not state.

### F9 — §4.6's stated premise (`Damage = ATK − DEF`) matches neither source branch

§4.6 (line 281) justifies the change as fixing a "straight-line armour
subtraction (`Damage = ATK - DEF`)". Tracing the source:

* **Classic** (`Server-Attack.cpp:985`) is *not* `ATK − DEF`; it is hyperbolic:
  `decpower = (dmg·ac / (2·dmg + 0.8·ac)) · 1.3`.
* **Renewal** already uses `GetDefensiveRate(...)` — a percentage, capped 90%
  (`tower_ttk.csv` renewal rates 39.13–48.14%).

The literal "1 unit" plateau does exist, but only through the *classic* floor
`validpower <= 0 → 1` (self-test: 100 dmg vs AC 2000 → 1); for any attack that
can clear `ac`, the classic curve already rises smoothly. So §4.6's real effect
is to **replace the renewal `GetDefensiveRate`** (dropping its level-difference
term and 90% cap) with a different percentage — not to fix a literal
subtraction. Which branch the rewrite targets decides whether §4.6 is a fix or a
regression.

### F10 — endgame raid boss still one-shots the tankiest reference player

`monster_mitigation.csv` (`KIND_DAEMA_GWIWANG`, `SortLevel` 2776). Effective
monster damage 125730–155730; proposed-mitigated 123065–152429 against an L200
reference player with 21780 HP. §4.2 proposes compressing endgame monster damage
to 2,000–4,000/hit; that work is not applied, and §4.8's monster-balance section
is marked `Status: PAUSED & ON HOLD` (line 311). The live data still carries the
~90k–120k base damage.

---

## What checks out

* Every §4.3 worked example reproduces exactly (48/48 self-test): STR, VIT-def,
  weight, HP, magic ratio, LUK cap, gear flat-def, elemental %.
* The §3.1/§3.2 monster stats match the real data (e.g. `KIND_TIGER` 10–18 / Str
  14 / AC 20; `KIND_BLUEFLY` 23–40 / Str 41 / AC 55; WindA 5).
* Monster STR→damage reproduces: `KIND_TIGER` eff 17–25, `KIND_BLUEFLY` eff
  45–62 (matches the audit's "50–60+" commentary).
* The legacy "MAG/4" shorthand is the average of the real min/max
  (`totalMag*(4000+Lv*50)/20000` and `(6000+Lv*50)/20000` → 0.20–0.30).
* LUK → +0.05%/pt with a +50% hard cap works as described (`luk.csv`).

## Guardrails before implementing §4

1. **Pin §4.6**: choose the `Level` owner and add an explicit floor/cap, then
   re-run `tower_ttk.csv` until R8/R10 land in the §4.8 bands.
2. **Co-tune the DEF budget** (§4.1 gear + §4.3 VIT-natural) against the §4.6
   divisor — as written, L100 DEF 771 gives ~12% mitigation.
3. **Decide the HP baseline** (true legacy `Vit*4` vs renewal) before quoting
   "same as before".
4. **Elemental**: multiply by the incoming element; do not apply unconditionally.
5. **Cap the stat damage term** so weapons keep meaning past L150.
6. **Weight**: confirm the intended high-level capacity (−34.8% at L100).
7. State the target generation (Classic vs Renewal); F9 hinges on it.

## Data note

`SortNUM − SortLevel` is **not** constant across joined monsters (offset
histogram `399×78, 1837×33, 1499×25, −1189×23 …`), so monster level must be read
from `charetcinfoE.SortLevel`; it cannot be derived from `charattackinfoE.SortNUM`.
Rows lacking a `SortLevel` (e.g. `KIND_Koryu_*`) fall back to the player level in
the legacy STR term and are flagged in the CSV.

## Limitations

* Proposed formulas are tested as written, against a *plausible* stat schedule
  interpolated from the audit's own examples — not a real character build.
* `monster_mitigation.csv` feeds the **same** DEF to both rules to isolate the
  formula change; it is not a full-fidelity before/after.
* True legacy player mitigation uses an equipment-derived DEF the simulator does
  not reconstruct; §4.6's flat-then-% pipeline (§4.1) is not applied on top of
  §4.6, which the audit leaves ambiguous (double-counting risk).
