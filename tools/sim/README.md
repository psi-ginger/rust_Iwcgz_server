# tools/sim — T-014 combat formula simulator

A stdlib-only Python simulator that applies the **legacy C++ combat formulas**
and the **proposed audit formulas** (`docs/rust-rewrite/reference/combat-mechanics-audit-baseline.md` §4)
to the **real deployed game data**, so proposed numbers can be inspected before
any game code is touched.

No numpy / matplotlib / pandas — the target box is low-spec Linux. Output is CSV
plus a text summary.

## Run

```powershell
cd goonzu-rs\tools\sim
python sim.py            # self-test, then writes out\*.csv + out\summary.txt
python sim.py --selftest # self-test only (writes nothing)
python sim.py --data-root "D:\some\GameServer"   # override data root
```

Data root defaults to
`<repo-root>\goonzu-server\deploy\GameServer` (resolved relative to this file),
overridable via `--data-root` or `GOONZU_DATA_ROOT`.

## Self-test

`sim.py` refuses to run the simulation unless the self-test passes. The
self-test pins every proposed formula to the **audit's own worked examples**
(e.g. §4.3: STR 550 @ L100 → 704; weight 550 @ L100 → 123800) and every legacy
formula to a value derived from the cited C++ (`Parametabox.cpp`,
`Server-Attack.cpp`, `ItemUnit.cpp`) or the real data rows.

A green harness proves nothing until it has been watched to go red. To verify:

```powershell
python -c "import sim, formulas; formulas.CONFIG['PROP_STR_BASE']=0.21; print(sim.selftest())"
```

That mutates one constant and should print `SELFTEST FAILED: 3/48 checks` and
return non-zero.

## Files

| file | what it is |
|---|---|
| `formulas.py` | `CONFIG` constants + legacy / proposed pure functions. Each constant cites its source. |
| `loaders.py` | UTF-16 TSV loaders for `charattackinfoE.txt`, `charetcinfoE.txt`, `ItemType*E.txt`; data-root resolution. |
| `sim.py` | self-test + the output sections + `out/summary.txt`. |
| `out/*.csv` | generated comparison tables (see below). |
| `report.md` | findings — what the numbers say about the proposal. |

## Data facts (verified on disk)

* `GameServer` data files are **UTF-16 (BOM `ff fe`) + tab separated**. Confirmed
  by reading the first bytes, not assumed.
* `charattackinfoE.txt`: 27 columns (`Char … MinDmg MaxDmg … AC Str … Wis`).
* `charetcinfoE.txt`: 24 columns (`Char Name SortLevel … FireA WaterA LandA WindA …`).
* `ItemTypeSwordE.txt`: 30-column positional schema (blueprint `04b` §3a).
* `ItemTypeArmourE.txt`: header row names the columns; `AC` is column 16 of 29.

## Outputs

| CSV | question it answers |
|---|---|
| `defense_curve.csv` | mitigation % vs DEF value, levels 1–100 (legacy vs proposed) |
| `defense_levelgap.csv` | same DEF under a level gap (legacy has a `(myLvl-enemyLvl)*200` term; proposed has none) |
| `hit_dodge.csv` | hit/dodge by level and AGI (legacy ignores dodge; proposed subtracts it) |
| `stat_curves.csv` | HP / MP / STR-ATK / MAG-ATK / natural-def / weight by level 1–200 |
| `swords.csv` | real swords: AttackDelay, min/max damage, legacy vs proposed STR term |
| `monster_mitigation.csv` | real monsters: raw→effective damage, incoming damage and hits-to-kill a reference player |
| `tower_ttk.csv` | Destiny-tower R8/R10 sanity vs §4.8 targets |
| `elemental.csv` | enchant grade × monster ElementA: legacy integer-division vs proposed % |
| `luk.csv` | LUK → crit damage / drop rate with the +50% cap |
| `mitigation_level_owner.csv` | §4.6's unspecified `Level`: same DEF under each reading |
| `summary.txt` | the exact numbers `report.md` cites |

## Caveats

* The proposed formulas are a **proposal, untested** (audit status). This tool
  measures them; it does not endorse them.
* `stat_schedule()` interpolates the audit's own §4.3 example stats
  (L1=10, L10=35, L30=120, L50=220, L100=550) and extrapolates above L100 at the
  50→100 slope. It is a plausible schedule, not a character build.
* In `monster_mitigation.csv`, the **same** player DEF is fed to both the legacy
  and proposed rules, to isolate the formula change. Real legacy DEF comes from a
  different stat path, so this is a formula-vs-formula comparison, not a
  full-fidelity before/after.
* Monster rows without a `charetcinfoE` level (e.g. `KIND_Koryu_*`) fall back to
  the player level in the legacy STR term; flagged per row.
