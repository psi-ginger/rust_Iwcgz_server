#!/usr/bin/env python3
"""T-014 -- combat formula simulator.

Applies the LEGACY C++ formulas and the PROPOSED audit formulas (audit
baseline section 4) to the real deployed game data, so the proposed numbers can
be eyeballed before any game code is touched.

Usage
-----
    python sim.py                 # self-test, then write out/*.csv + out/summary.txt
    python sim.py --selftest      # self-test only (no files written)
    python sim.py --data-root DIR # override the GameServer data root
    python sim.py --out-dir DIR   # override the output directory

The self-test *must* pass before the simulator will run. A green simulator that
was never watched to go red proves nothing; run ``--selftest`` and break one
constant in formulas.py to see it fail.

Stdlib only -- no numpy/matplotlib on the target box.
"""

from __future__ import annotations

import argparse
import csv
import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import formulas as F
import loaders as L

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "out")

# stat anchors straight from the audit baseline section 4.3 worked examples
#   level -> stat value, shared by STR / MAG / VIT illustrations
STAT_ANCHORS = [(1, 10), (10, 35), (30, 120), (50, 220), (100, 550)]
ANCHOR_SLOPE = (550 - 220) / (100 - 50)  # extrapolate above level 100


def stat_schedule(level: int) -> int:
    """Piecewise-linear interpolation through the audit's own example stats."""
    if level <= 1:
        return 10
    for (l0, s0), (l1, s1) in zip(STAT_ANCHORS, STAT_ANCHORS[1:]):
        if level <= l1:
            t = (level - l0) / (l1 - l0)
            return int(round(s0 + t * (s1 - s0)))
    extra = level - 100
    return int(round(550 + extra * ANCHOR_SLOPE))


def per_hit(damage: int, rate_1e4: int) -> int:
    """Apply a 1/10000 reduction rate, floor 1 (matches GetDamage)."""
    return max(1, damage - (damage * rate_1e4) // 10000)


def ceil_div(a: int, b: int) -> int:
    return -(-a // b) if b > 0 else 0


# ---------------------------------------------------------------------------
# self-test -- locks the harness to the cited source / audit examples
# ---------------------------------------------------------------------------

def selftest() -> int:
    checks = [
        # proposed STR damage -- audit section 4.3 item 5 worked examples
        ("str 10@L1", F.proposed_str_damage(10, 1), 2),
        ("str 35@L10", F.proposed_str_damage(35, 10), 15),
        ("str 120@L30", F.proposed_str_damage(120, 30), 73),
        ("str 220@L50", F.proposed_str_damage(220, 50), 176),
        ("str 550@L100", F.proposed_str_damage(550, 100), 704),
        # proposed VIT natural def -- section 4.3 item 4 worked examples
        ("vitdef 35@L10", F.proposed_natural_def(35, 10), 13),
        ("vitdef 120@L30", F.proposed_natural_def(120, 30), 67),
        ("vitdef 220@L50", F.proposed_natural_def(220, 50), 165),
        ("vitdef 550@L100", F.proposed_natural_def(550, 100), 677),
        # proposed weight -- section 4.3 item 8 worked examples
        ("weight 10@L1", F.proposed_weight(10, 1), 5008),
        ("weight 35@L10", F.proposed_weight(35, 10), 11080),
        ("weight 120@L30", F.proposed_weight(120, 30), 30240),
        ("weight 220@L50", F.proposed_weight(220, 50), 52400),
        ("weight 550@L100", F.proposed_weight(550, 100), 123800),
        # proposed HP -- section 4.3 item 4 (Lv100: 100 VIT = 1300 HP)
        ("hp 100@L100", F.proposed_hp(100, 100), 1300),
        # proposed magical ratio boundaries (section 4.3 item 6: base 0.28, +0.01/Lv)
        ("mag 100@L1", F.proposed_mag_damage(100, 1), 28),
        ("mag 100@L10", F.proposed_mag_damage(100, 10), 42),
        # proposed mitigation -- section 4.6: DEF=700, Lv=0 -> exactly 50%
        ("mitig 700@L0", F.proposed_mitigation(700, 0), 5000),
        ("mitig 0@L50", F.proposed_mitigation(0, 50), 0),
        # proposed gear flat def -- section 4.1: 4 + 0.9*Lv
        ("geardef L100", F.proposed_gear_flat_def(100), 94.0),
        ("geardef L1", F.proposed_gear_flat_def(1), 4.9),
        # proposed enchant -- section 4.1 item 2: 6% per grade
        ("ench pct g1", F.proposed_enchant_reduction_pct(1), 6),
        ("ench pct g2", F.proposed_enchant_reduction_pct(2), 12),
        ("ench pct g10", F.proposed_enchant_reduction_pct(10), 60),
        # proposed LUK -- section 4.3 item 3, +0.05%/pt, +50% cap
        ("luk 100", F.proposed_luk_crit_pct(100), 5.0),
        ("luk 1000", F.proposed_luk_crit_pct(1000), 50.0),
        ("luk 2000 capped", F.proposed_luk_crit_pct(2000), 50.0),
        # legacy HP/MP -- parametabox.h VIT_LIFE_RATE / INT_MANA_RATE = 4
        ("legacy hp vit5", F.legacy_hp(5), 20),
        ("legacy mp mag5", F.legacy_mp(5), 20),
        # renewal HP -- blueprint 02 section 2.1: Vit*(8000+level*5)/1000
        ("renewal hp vit5@L1", F.renewal_hp(5, 1), 40),
        # legacy weight -- Parametabox.cpp:3641  (STR*200 + level*800)
        ("legacy weight 10@L1", F.legacy_weight(10, 1), 2800),
        ("legacy weight 550@L100", F.legacy_weight(550, 100), 190000),
        # legacy STR damage -- real monsters (charattackinfoE rows)
        ("tiger strdmg", F.legacy_str_damage(14, 1000, 10), 7),
        ("bluefly strdmg", F.legacy_str_damage(41, 1000, 17), 22),
        # legacy MAG damage -- Parametabox.cpp SetMagicalAttackDemage (min 0.20, max 0.30)
        ("mag dmg 100@L1", F.legacy_mag_damage(100, 1), (20, 30)),
        # legacy defensive rate -- caps and level-gap term
        ("defrate 0 -> 10% floor", F.legacy_defensive_rate(0, 1, 1), 1000),
        ("defrate 100000 -> 90% cap", F.legacy_defensive_rate(100000, 1, 1), 9000),
        ("defrate 2000 even", F.legacy_defensive_rate(2000, 100, 100), 7407),
        # legacy classic mitigation -- Server-Attack.cpp:985
        ("classic tick-1", F.legacy_classic_mitigate(100, 2000), 1),
        ("classic 1000 vs 2000", F.legacy_classic_mitigate(1000, 2000), 279),
        # legacy enchant integer-division bug -- audit section 2.2
        ("ench g1 elem5", F.legacy_enchant_reduction(1, 5), 0),
        ("ench g2 elem5", F.legacy_enchant_reduction(2, 5), 0),
        ("ench g3 elem5", F.legacy_enchant_reduction(3, 5), 0),
        ("ench g6 elem5", F.legacy_enchant_reduction(6, 5), 1),
        ("ench g10 elem170", F.legacy_enchant_reduction(10, 170), 85),
        # legacy hit -- Parametabox.cpp GetHitRate
        ("hit hnd10 L1", F.legacy_hit_rate(10, 1, 1), 7186),
        # milestone helper
        ("milestone L100", F.milestone(100), 50),
        ("milestone L9", F.milestone(9), 0),
    ]
    bad = [(name, got, want) for name, got, want in checks if got != want]
    for name, got, want in checks:
        status = "ok  " if got == want else "FAIL"
        if got != want:
            print("  %s %-24s got=%r want=%r" % (status, name, got, want))
    if bad:
        print("\nSELFTEST FAILED: %d/%d checks" % (len(bad), len(checks)))
        return 1
    print("selftest: %d/%d checks passed" % (len(checks), len(checks)))
    return 0


# ---------------------------------------------------------------------------
# output sections
# ---------------------------------------------------------------------------

def write_csv(path, header, rows):
    with open(path, "w", newline="", encoding="utf-8-sig") as fh:
        w = csv.writer(fh)
        w.writerow(header)
        w.writerows(rows)
    print("  wrote %-28s %d rows" % (os.path.basename(path), len(rows)))


def section_defense(out):
    rows = []
    for level in (1, 10, 30, 50, 70, 85, 100):
        for d in range(0, 3001, 50):
            leg = F.legacy_defensive_rate(d, level, level)
            pro = F.proposed_mitigation(d, level)
            rows.append([level, d, round(leg / 100.0, 2), round(pro / 100.0, 2)])
    write_csv(os.path.join(out, "defense_curve.csv"),
              ["level", "def_value", "legacy_rate_pct", "proposed_rate_pct"], rows)

    # level-gap: legacy applies (myLvl-enemyLvl)*200; proposed has no enemy level.
    rows = []
    for d in (500, 1000, 2000, 4000):
        for enemy in (10, 50, 70, 85, 100, 150, 200):
            leg = F.legacy_defensive_rate(d, 70, enemy)
            pro = F.proposed_mitigation(d, 70)
            rows.append([d, 70, enemy, round(leg / 100.0, 2), round(pro / 100.0, 2)])
    write_csv(os.path.join(out, "defense_levelgap.csv"),
              ["def_value", "player_level", "enemy_level", "legacy_rate_pct", "proposed_rate_pct"], rows)


def section_hit_dodge(out):
    rows = []
    cases = [
        # (atk_lvl, def_lvl, atk_agi, def_agi, label)
        (10, 10, 15, 15, "even"),
        (50, 50, 30, 30, "even"),
        (50, 100, 30, 30, "player -50 lv"),
        (100, 50, 30, 30, "player +50 lv"),
        (100, 200, 50, 50, "player -100 lv"),
        (50, 85, 20, 60, "low agi vs heavy dodge"),
        (50, 85, 60, 20, "high agi vs low dodge"),
    ]
    for atk_lvl, def_lvl, aa, da, label in cases:
        leg_hit = F.legacy_hit_rate(aa, atk_lvl, def_lvl)
        leg_dodge = F.legacy_dodge_rate(da, def_lvl, atk_lvl)
        # legacy code uses hit alone; dodge is computed but not subtracted (audit section 4.3 item 2)
        leg_effective = leg_hit
        # proposed: section 4.7 removes the level-gap term, section 4.3 item 2 subtracts dodge.
        c = F.CONFIG
        pro_hit = min(c["HIT_CAP"], c["HIT_BASE"] + (aa * c["PHYS_RANK_MULT"] + atk_lvl) * c["HIT_RANK_SCALE"])
        pro_dodge = min(c["DODGE_CAP"], (da * c["PHYS_RANK_MULT"] + def_lvl) * c["DODGE_RANK_SCALE"])
        pro_effective = max(0, min(10000, pro_hit - pro_dodge))
        rows.append([label, atk_lvl, def_lvl, aa, da,
                     round(leg_hit / 100.0, 2), round(leg_dodge / 100.0, 2),
                     round(leg_effective / 100.0, 2), round(pro_effective / 100.0, 2)])
    write_csv(os.path.join(out, "hit_dodge.csv"),
              ["case", "atk_level", "def_level", "atk_agi", "def_agi",
               "legacy_hit_pct", "legacy_dodge_pct", "legacy_effective_hit_pct",
               "proposed_effective_hit_pct"], rows)


def section_stat_curves(out):
    rows = []
    for level in list(range(1, 101)) + [120, 150, 200]:
        s = stat_schedule(level)
        leg_min, leg_max = F.legacy_mag_damage(s, level)
        rows.append([
            level, s,
            F.legacy_hp(s), F.renewal_hp(s, level), F.proposed_hp(s, level),
            F.legacy_mp(s), F.renewal_mp(s, level),
            F.legacy_str_damage(s, 1000, level), F.proposed_str_damage(s, level),
            leg_min, leg_max, F.proposed_mag_damage(s, level),
            F.proposed_natural_def(s, level), F.proposed_gear_flat_def(level),
            F.proposed_wis_def(s, level),
            F.legacy_weight(s, level), F.proposed_weight(s, level),
        ])
    write_csv(os.path.join(out, "stat_curves.csv"),
              ["level", "stat", "legacy_hp", "renewal_hp", "proposed_hp",
               "legacy_mp", "renewal_mp", "legacy_str_dmg_sword1000",
               "proposed_str_dmg", "legacy_mag_min", "legacy_mag_max",
               "proposed_mag_dmg", "proposed_natural_def", "proposed_gear_def",
               "proposed_wis_magic_def", "legacy_weight", "proposed_weight"],
              rows)


def section_swords(out, data_root):
    path = os.path.join(data_root, "Item", "Data", "ItemTypeSwordE.txt")
    if not os.path.exists(path):
        print("  (skip swords: %s missing)" % path)
        return
    rows = []
    for it in L.load_swords(path):
        try:
            lvl = int(it["Level"])
            delay = int(it["AttackDelay"])
            mn = int(it["MinDamage"])
            mx = int(it["MaxDamage"])
        except (ValueError, KeyError):
            continue
        if lvl <= 0 or delay <= 0:
            continue
        s = stat_schedule(lvl)
        rows.append([it["Name"], lvl, delay, mn, mx, it["Grade"],
                     F.legacy_str_damage(s, delay, lvl), F.proposed_str_damage(s, lvl)])
    write_csv(os.path.join(out, "swords.csv"),
              ["name", "level", "attack_delay", "min_damage", "max_damage", "grade",
               "legacy_str_dmg", "proposed_str_dmg"], rows)


def _join_monsters(data_root):
    atk = L.load_char_attack(os.path.join(data_root, "CharInfo", "charattackinfoE.txt"))
    etc = L.load_char_etc(os.path.join(data_root, "CharInfo", "charetcinfoE.txt"))
    etc_by = {r["Char"]: r for r in etc}
    joined = []
    for r in atk:
        e = etc_by.get(r["Char"])
        joined.append((r, e))
    return joined


def section_monsters(out, data_root):
    joined = _join_monsters(data_root)
    sample = [
        "KIND_TIGER", "KIND_LOCUST", "KIND_LADYBUG", "KIND_STAGBEETLE",
        "KIND_FRUITBROS", "KIND_VINE", "KIND_TREEMON", "KIND_BLUEFLY",
        "KIND_MONKEYSON", "KIND_NINJAPANDA", "KIND_GOTPA", "KIND_WHITEWOLF_2",
        "KIND_EMPERORSOLDIER", "KIND_GREATWALLSOLDIER", "KIND_Ancient_Xingshi01",
        "KIND_Ancient_Xingshi06", "KIND_Koryu_CRABGURI", "KIND_Koryu_SAMURAIBoss",
        "KIND_DAEMA_GWIWANG",
    ]
    by = {r["Char"]: (r, e) for (r, e) in joined}
    # join-integrity: is SortLevel == SortNUM - 399?
    offsets = {}
    for r, e in joined:
        if e and str(r.get("SortNUM", "")).lstrip("-").isdigit() and str(e.get("SortLevel", "")).lstrip("-").isdigit():
            off = int(r["SortNUM"]) - int(e["SortLevel"])
            offsets[off] = offsets.get(off, 0) + 1
    print("  monster join: %d attackinfo rows, %d with charetcinfo, offset histogram=%s"
          % (len(joined), sum(1 for _, e in joined if e), sorted(offsets.items(), key=lambda kv: -kv[1])[:4]))

    rows = []
    player_levels = [10, 30, 50, 70, 85, 100, 150, 200]
    for name in sample:
        pair = by.get(name)
        if not pair:
            continue
        r, e = pair
        try:
            base_min = int(r["MinDmg"]); base_max = int(r["MaxDmg"])
            interval = int(r["Attack_Interval"]); stre = int(r["Str"]); ac = int(r["AC"])
        except (ValueError, KeyError):
            continue
        mon_level = int(e["SortLevel"]) if e and str(e.get("SortLevel", "")).isdigit() else None
        for plvl in player_levels:
            pvit = stat_schedule(plvl)
            pdef = F.proposed_gear_flat_def(plvl) + F.proposed_natural_def(pvit, plvl)
            php = F.proposed_hp(pvit, plvl)
            if mon_level is not None:
                strd = F.legacy_str_damage(stre, interval, mon_level)
            else:
                strd = F.legacy_str_damage(stre, interval, plvl)
            eff_min = base_min + strd
            eff_max = base_max + strd
            # legacy monster->player: GetDefensiveRate(same DEF, myLvl=player, enemyLvl=monster)
            leg_rate = F.legacy_defensive_rate(int(pdef), plvl, mon_level if mon_level is not None else plvl)
            pro_rate = F.proposed_mitigation(pdef, plvl)
            leg_in_min = per_hit(eff_min, leg_rate)
            pro_in_min = per_hit(eff_min, pro_rate)
            pro_in_max = per_hit(eff_max, pro_rate)
            rows.append([
                name, r.get("Name", ""), mon_level if mon_level is not None else "",
                base_min, base_max, stre, interval, strd, eff_min, eff_max, ac,
                pvit, round(pdef, 1),
                round(leg_rate / 100.0, 2), round(pro_rate / 100.0, 2),
                leg_in_min, pro_in_min, pro_in_max, php,
                ceil_div(php, pro_in_max) if pro_in_max else "",
            ])
    write_csv(os.path.join(out, "monster_mitigation.csv"),
              ["char", "name", "mon_level", "base_min", "base_max", "str",
               "attack_interval", "str_dmg", "eff_min", "eff_max", "ac",
               "player_vit", "player_def", "legacy_rate_pct", "proposed_rate_pct",
               "legacy_incoming_min", "proposed_incoming_min", "proposed_incoming_max",
               "player_hp", "proposed_hits_to_kill"], rows)


def section_tower(out):
    rows = []
    r8 = 850
    r10 = 1100
    for weapon, dmg in (("R8", r8), ("R10", r10)):
        for ac in (450, 550, 650):
            for hp in (6000, 7500, 9000):
                # legacy classic (current player->monster path in Classic mode)
                lc_dmg = F.legacy_classic_mitigate(dmg, ac)
                # legacy renewal: GetDefensiveRate(monster DEF, playerLvl, monsterLvl)
                lr_rate = F.legacy_defensive_rate(ac, 80, 80)
                lr_dmg = per_hit(dmg, lr_rate)
                # proposed section 4.6
                pr_rate = F.proposed_mitigation(ac, 80)
                pr_dmg = per_hit(dmg, pr_rate)
                rows.append([
                    weapon, dmg, ac, hp,
                    round(lc_dmg, 1), ceil_div(hp, int(lc_dmg)),
                    round(lr_rate / 100.0, 2), lr_dmg, ceil_div(hp, lr_dmg),
                    round(pr_rate / 100.0, 2), pr_dmg, ceil_div(hp, pr_dmg),
                ])
    write_csv(os.path.join(out, "tower_ttk.csv"),
              ["weapon", "raw_dmg", "monster_ac", "monster_hp",
               "legacy_classic_dmg", "legacy_classic_hits",
               "legacy_renewal_rate_pct", "legacy_renewal_dmg", "legacy_renewal_hits",
               "proposed_rate_pct", "proposed_dmg", "proposed_hits"], rows)


def section_elemental(out):
    rows = []
    for elem in (5, 8, 15, 50, 170):
        for grade in range(0, 11):
            leg = F.legacy_enchant_reduction(grade, elem)
            pct = F.proposed_enchant_reduction_pct(grade)
            rows.append([grade, elem, leg, pct, round(pct / 100.0 * 300, 1)])
    write_csv(os.path.join(out, "elemental.csv"),
              ["grade", "monster_element_a", "legacy_flat_reduction",
               "proposed_reduction_pct", "proposed_flat_at_300_dmg"], rows)


def section_luk(out):
    rows = []
    for luk in list(range(0, 1601, 50)):
        pct = F.proposed_luk_crit_pct(luk)
        rows.append([luk, round(pct, 2), round(150 + pct, 2), round(pct, 2)])
    write_csv(os.path.join(out, "luk.csv"),
              ["luk", "crit_dmg_bonus_pct", "crit_multiplier_pct", "drop_rate_bonus_pct"], rows)


def section_mitigation_owner(out):
    """Section 4.6 writes 'DEF + 700 + (Level x 50)' without saying whose level.

    Emit the same DEF under each plausible reading so the ambiguity is visible.
    """
    rows = []
    cases = [
        ("L100 player DEF=771 vs L78 monster", 771, [("Level=0 (ignored)", 0), ("monster level", 78), ("player level", 100)]),
        ("L80 R8 target vs AC=550", 550, [("Level=0 (ignored)", 0), ("monster level 80", 80), ("player level 80", 80)]),
    ]
    for label, defv, readings in cases:
        for name, lvl in readings:
            rows.append([label, defv, name, lvl, round(F.proposed_mitigation(defv, lvl) / 100.0, 2)])
    write_csv(os.path.join(out, "mitigation_level_owner.csv"),
              ["case", "def_value", "level_reading", "level_used", "proposed_rate_pct"], rows)


def write_summary(out, data_root):
    """Emit the exact numbers report.md cites, so no figure is typed from memory."""
    lines = []
    add = lines.append
    add("T-014 combat simulator summary (generated by sim.py)")
    add("data root: %s" % data_root)
    add("")
    add("[defense curve] mitigation % for the same DEF, level 100, DEF=1000:")
    add("  legacy  = %.2f%%   proposed = %.2f%%"
        % (F.legacy_defensive_rate(1000, 100, 100) / 100.0, F.proposed_mitigation(1000, 100) / 100.0))
    add("  legacy floor/cap = %.1f%%/%.1f%%   proposed has no explicit cap"
        % (F.CONFIG["DEF_RATE_MIN"] / 100.0, F.CONFIG["DEF_RATE_MAX"] / 100.0))
    add("")
    add("[stat curve] level-1 HP: legacy(Vit*4)=%d renewal=%d proposed=%d"
        % (F.legacy_hp(10), F.renewal_hp(10, 1), F.proposed_hp(10, 1)))
    add("[stat curve] level-100 VIT=%d HP: legacy=%d renewal=%d proposed=%d"
        % (stat_schedule(100), F.legacy_hp(stat_schedule(100)), F.renewal_hp(stat_schedule(100), 100), F.proposed_hp(stat_schedule(100), 100)))
    add("")
    add("[weight] level-100 STR=%d: legacy=%d proposed=%d (proposed is %+.1f%%)"
        % (stat_schedule(100), F.legacy_weight(stat_schedule(100), 100), F.proposed_weight(stat_schedule(100), 100),
           100.0 * (F.proposed_weight(stat_schedule(100), 100) - F.legacy_weight(stat_schedule(100), 100)) / F.legacy_weight(stat_schedule(100), 100)))
    add("[weight] level-1 STR=%d: legacy=%d proposed=%d"
        % (stat_schedule(1), F.legacy_weight(stat_schedule(1), 1), F.proposed_weight(stat_schedule(1), 1)))
    add("")
    add("[tower] R8=850 vs AC=550/HP=7500, player level 80:")
    add("  classic   dmg=%d hits=%d" % (F.legacy_classic_mitigate(850, 550), ceil_div(7500, F.legacy_classic_mitigate(850, 550))))
    r = F.legacy_defensive_rate(550, 80, 80)
    add("  renewal   dmg=%d hits=%d (rate %.2f%%)" % (per_hit(850, r), ceil_div(7500, per_hit(850, r)), r / 100.0))
    p = F.proposed_mitigation(550, 80)
    add("  proposed  dmg=%d hits=%d (rate %.2f%%)" % (per_hit(850, p), ceil_div(7500, per_hit(850, p)), p / 100.0))
    add("  audit target: 800-900 raw -> 500-650 per hit, 7-10 hits")
    _k = F.CONFIG["PROP_DEF_DIVISOR_BASE"] + 80 * F.CONFIG["PROP_DEF_LEVEL_SCALE"]

    def _ac_for_per_hit(raw, want):
        r = (raw - want) * 10000.0 / raw          # rate in 1/10000
        return r * _k / (10000.0 - r)

    add("  section 4.6 needs AC %.0f (650/hit) .. %.0f (500/hit) at L80, but section 4.8 specifies AC 450-650"
        % (_ac_for_per_hit(850, 650), _ac_for_per_hit(850, 500)))
    add("")
    add("[mitigation] section 4.6 does not say whose 'Level'; DEF=771 (L100 player) gives:")
    for _name, _lvl in (("Level=0 (ignored)", 0), ("monster level", 78), ("player level", 100)):
        add("   %-18s -> %.2f%%" % (_name, F.proposed_mitigation(771, _lvl) / 100.0))
    add("")
    add("[elemental] ElementA=5: legacy reduction grade1=%d grade2=%d grade3=%d grade6=%d"
        % (F.legacy_enchant_reduction(1, 5), F.legacy_enchant_reduction(2, 5),
           F.legacy_enchant_reduction(3, 5), F.legacy_enchant_reduction(6, 5)))
    add("  proposed: grade1=%.0f%% grade10=%.0f%% (unconditional)"
        % (F.proposed_enchant_reduction_pct(1), F.proposed_enchant_reduction_pct(10)))
    add("")
    add("[tower targets vs source AC] the legacy AC values in charattackinfoE are raw monster defences;")
    add("  the classified tower AC 450-650 must be compared against the proposed formula's own scale.")

    text = "\n".join(lines)
    with open(os.path.join(out, "summary.txt"), "w", encoding="utf-8") as fh:
        fh.write(text + "\n")
    print("  wrote summary.txt")
    return text


def main(argv=None):
    ap = argparse.ArgumentParser(description="T-014 combat formula simulator")
    ap.add_argument("--data-root", default=L.default_data_root())
    ap.add_argument("--out-dir", default=OUT)
    ap.add_argument("--selftest", action="store_true", help="run the self-test only")
    args = ap.parse_args(argv)

    if selftest() != 0:
        return 1
    if args.selftest:
        return 0

    data_root = args.data_root
    if not os.path.isdir(data_root):
        print("error: data root not found: %s" % data_root, file=sys.stderr)
        print("       pass --data-root or set GOONZU_DATA_ROOT", file=sys.stderr)
        return 2
    os.makedirs(args.out_dir, exist_ok=True)
    out = args.out_dir
    print("data root: %s" % data_root)
    section_defense(out)
    section_hit_dodge(out)
    section_stat_curves(out)
    section_swords(out, data_root)
    section_monsters(out, data_root)
    section_tower(out)
    section_elemental(out)
    section_luk(out)
    section_mitigation_owner(out)
    text = write_summary(out, data_root)
    print()
    print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
