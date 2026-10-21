"""T-014 combat formula simulator -- legacy vs proposed formulas.

Every constant in this file is either:
  (a) transcribed from the C++ source (with file:line citation), or
  (b) transcribed from the audit baseline (with section citation).

No number is typed from memory. If a constant looks wrong, fix it against the
cited source, not against this file's prose.

Sources
-------
LEGACY  : ``GoonZu Source Code\\Server_Core\\GoonZu_Source``
          - common\\Char\\ParametaBox\\Parametabox.cpp
          - common\\Char\\ParametaBox\\parametabox.h
          - common\\Item\\ItemCommon\\ItemUnit.cpp
          - Server\\Server-Attack.cpp
PROPOSED: docs\\rust-rewrite\\reference\\combat-mechanics-audit-baseline.md  (status: proposal, untested)
"""

from __future__ import annotations

# ---------------------------------------------------------------------------
# CONFIG -- the single block of tunables / constants
# ---------------------------------------------------------------------------

CONFIG = {
    # -- legacy: common\Char\ParametaBox\parametabox.h:11-12
    "VIT_LIFE_RATE": 4,          # Life = Vit * 4
    "INT_MANA_RATE": 4,          # Mana = Int * 4

    # -- legacy: Parametabox.cpp GetDefensiveRate (:6949-6990)
    #    siTotalPHysicalDefensiveRate = ((def*10000)/(def+700)) * (10000 + (myLvl-enemyLvl)*200) / 10000
    "DEF_DIVISOR_BASE": 700,
    "DEF_LEVEL_DELTA_SCALE": 200,
    "DEF_RATE_MIN": 1000,        # 10% floor
    "DEF_RATE_MAX": 9000,        # 90% cap

    # -- legacy: Parametabox.cpp GetHitRate (:7020-7100), GetDodgeRate (:7158-7260)
    #    hit  = min(10000, (7000 + rank*6) * (10000 + (myLvl-enemyLvl)*50) / 10000)
    #    dodge= min( 5000,             rank*6  * (10000 + (myLvl-enemyLvl)*50) / 10000)
    "HIT_BASE": 7000,
    "HIT_RANK_SCALE": 6,
    "DODGE_RANK_SCALE": 6,
    "HIT_DODGE_LEVEL_SCALE": 50,
    "HIT_CAP": 10000,
    "DODGE_CAP": 5000,

    # -- legacy: Parametabox.cpp SetPHysicalHitRate (:2141) / SetPHysicalDodgeRate (:3199)
    #    physical hit rank = Hnd*3 + level ; physical dodge rank = Dex*3 + level
    #    (NOTE: hit is driven by Handicraft, not Dex -- audit section 4.3 item 2)
    "PHYS_RANK_MULT": 3,
    # -- legacy: SetMagicalHitRate (:2262) / SetMagicalDodgeRate (:3343)
    #    magical hit rank = Wis*2 + level ; magical dodge rank = Dex*2 + level
    "MAG_HIT_MULT": 2,
    "MAG_DODGE_MULT": 2,

    # -- legacy: Parametabox.cpp SetPhysicalAttackDemage (:452-696)
    #    siStrPHysicalAttackDmg = totalStr * ((dwAttackInterval*10) + (level*50)) / 20000
    #    minDmg = defaultweaponmindamage + siStrPHysicalAttackDmg   (same added to max)
    "ATK_INTERVAL_MULT": 10,
    "ATK_LEVEL_SCALE": 50,
    "ATK_DIVISOR": 20000,

    # -- legacy: Parametabox.cpp SetMagicalAttackDemage (:1389-...)
    #    minDmg = totalMag * (4000 + level*50)/20000
    #    maxDmg = totalMag * (6000 + level*50)/20000
    "MAG_MIN_BASE": 4000,
    "MAG_MAX_BASE": 6000,
    "MAG_LEVEL_SCALE": 50,

    # -- legacy: Server-Attack.cpp GetDamage classic branch (:985)
    #    decpower = ((dmg*ac) / ((dmg*2) + ac*8/10)) * 1.3
    "CLASSIC_AC_SCALE": 8,       # ac*8/10
    "CLASSIC_AC_DIV": 10,
    "CLASSIC_DEC_MULT": 1.3,
    "VALIDPOWER_FLOOR": 1,

    # -- legacy: Parametabox.cpp SetWeight (:3641)
    #    siStrWeigt = (totalStr * 200) + (level * 800)
    "LEGACY_WEIGHT_STR": 200,
    "LEGACY_WEIGHT_LEVEL": 800,

    # -- legacy: ItemUnit.cpp GetEnchantACFrom (:2073-2113) -- siEnchantDef per grade 0..10
    "ENCHANT_DEF": [0, 5, 6, 8, 11, 15, 20, 26, 33, 41, 50],
    # -- legacy: Server-Attack.cpp (audit section 2.2)
    #    enchantac = (enchantPercent * monster_ElementA) / 100   (integer division)
    "ENCHANT_ELEMENT_DIV": 100,

    # ---- proposed (audit section 4) -------------------------------------
    # section 4.1: flat gear def = sum(base 1 + weight*itemLevel)
    "PROP_GEAR_ARMOR_W": 0.4,    # armor  1 + 0.4*Lv
    "PROP_GEAR_HELM_W": 0.2,     # helmet 1 + 0.2*Lv
    "PROP_GEAR_BOOTS_W": 0.2,    # boots  1 + 0.2*Lv
    "PROP_GEAR_BELT_W": 0.1,     # belt   1 + 0.1*Lv
    "PROP_GEAR_PIECES_BASE": 4,  # 4 pieces, 1 point each

    # section 4.3 item 4: VIT natural def
    "PROP_VIT_DEF_BASE": 0.15,
    "PROP_VIT_DEF_PER_LEVEL": 0.01,
    "PROP_MILESTONE_STEP": 10,
    "PROP_MILESTONE_BONUS": 5,

    # section 4.3 item 5/6: STR / MAG damage
    "PROP_STR_BASE": 0.20,
    "PROP_MAG_BASE": 0.28,
    "PROP_STAT_PER_LEVEL": 0.01,

    # section 4.3 item 4: HP = floor(VIT * (8.0 + 0.05*Level))
    "PROP_HP_BASE": 8.0,
    "PROP_HP_PER_LEVEL": 0.05,
    # renewal (HealthAddChange) HP: VIT * (8000 + level*5)/1000  (blueprint 02 section 2.1)
    # stock-binary baseline: since SPEC_CLIENT_REBUILD_PATCH.md §11 the shipped client follows proposed_hp, not this column.
    "RENEWAL_HP_BASE": 8000,
    "RENEWAL_HP_LEVEL": 5,
    "RENEWAL_HP_DIV": 1000,

    # section 4.3 item 7: WIS magical defense = (WIS + Level) * 0.90 ; magical rank = WIS*2 + level
    "PROP_WIS_DEF_MULT": 0.90,

    # section 4.6: unified mitigation = DEF*10000 / (DEF + 700 + Level*50)
    "PROP_DEF_DIVISOR_BASE": 700,
    "PROP_DEF_LEVEL_SCALE": 50,

    # section 4.1 item 2: elemental direct % reduction, 6% per grade
    "PROP_ENCHANT_PCT_PER_GRADE": 6,

    # section 4.3 item 3: LUK
    "PROP_LUK_CRIT_PCT_PER_POINT": 0.05,
    "PROP_LUK_CAP_PCT": 50.0,

    # section 4.3 item 8: weight = 3000 + STR*200 + Level*8 + 1000*floor(Level/10)
    "PROP_WEIGHT_BASE": 3000,
    "PROP_WEIGHT_STR": 200,
    "PROP_WEIGHT_LEVEL": 8,
    "PROP_WEIGHT_MILESTONE": 1000,

    # section 4.8: destiny tower targets
    "TOWER_R8_DMG": (800, 900),
    "TOWER_R10_DMG": (1000, 1200),
    "TOWER_MOB_HP": (6000, 9000),
    "TOWER_MOB_AC": (450, 650),
    "TOWER_MOB_ATK": (200, 320),
    "TOWER_MOB_LEVEL": (70, 85),
    "TOWER_R8_HITS": (7, 10),
    "TOWER_R10_HITS": (5, 6),
}


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------

def milestone(level: int) -> int:
    """5 * floor(level/10) -- section 4.3 (shared by STR/MAG/VIT)."""
    return CONFIG["PROP_MILESTONE_BONUS"] * (level // CONFIG["PROP_MILESTONE_STEP"])


def _idiv(a: int, b: int) -> int:
    """C++ integer division (truncate toward zero). Inputs here are >= 0."""
    return int(a / b)


# ---------------------------------------------------------------------------
# LEGACY formulas
# ---------------------------------------------------------------------------

def legacy_defensive_rate(def_value: int, my_level: int, enemy_level: int) -> int:
    """Parametabox.cpp:GetDefensiveRate physical branch.

    Returns the reduction rate in 1/10000 units (per the source), clamped
    [1000, 9000]. Both physical and magical use the same shape (only the
    defensive stat differs); this models the physical branch.
    """
    base = _idiv(def_value * 10000, def_value + CONFIG["DEF_DIVISOR_BASE"])
    scaled = _idiv(base * (10000 + (my_level - enemy_level) * CONFIG["DEF_LEVEL_DELTA_SCALE"]), 10000)
    return max(CONFIG["DEF_RATE_MIN"], min(CONFIG["DEF_RATE_MAX"], scaled))


def legacy_classic_mitigate(damage: int, ac: int) -> int:
    """Server-Attack.cpp:985 classic branch. Returns final damage (>=1)."""
    if damage + ac == 0:
        dec = 0
    else:
        denom = (damage * 2) + _idiv(ac * CONFIG["CLASSIC_AC_SCALE"], CONFIG["CLASSIC_AC_DIV"])
        dec = int(_idiv(damage * ac, denom) * CONFIG["CLASSIC_DEC_MULT"])
    out = damage - dec
    return max(CONFIG["VALIDPOWER_FLOOR"], out)


def legacy_str_damage(total_str: int, attack_interval: int, level: int) -> int:
    """Parametabox.cpp:SetPhysicalAttackDemage STR contribution."""
    return _idiv(
        total_str * ((attack_interval * CONFIG["ATK_INTERVAL_MULT"]) + (level * CONFIG["ATK_LEVEL_SCALE"])),
        CONFIG["ATK_DIVISOR"],
    )


def legacy_mag_damage(total_mag: int, level: int) -> tuple:
    """Parametabox.cpp:SetMagicalAttackDemage min/max."""
    lo = _idiv(total_mag * (CONFIG["MAG_MIN_BASE"] + level * CONFIG["MAG_LEVEL_SCALE"]), CONFIG["ATK_DIVISOR"])
    hi = _idiv(total_mag * (CONFIG["MAG_MAX_BASE"] + level * CONFIG["MAG_LEVEL_SCALE"]), CONFIG["ATK_DIVISOR"])
    return lo, hi


def legacy_hp(total_vit: int) -> int:
    """parametabox.h: Life = Vit * VIT_LIFE_RATE."""
    return total_vit * CONFIG["VIT_LIFE_RATE"]


def legacy_mp(total_mag: int) -> int:
    return total_mag * CONFIG["INT_MANA_RATE"]


def renewal_hp(total_vit: int, level: int) -> int:
    """blueprint 02 section 2.1: Vit * (8000 + level*5)/1000."""
    return _idiv(total_vit * (CONFIG["RENEWAL_HP_BASE"] + level * CONFIG["RENEWAL_HP_LEVEL"]), CONFIG["RENEWAL_HP_DIV"])


def renewal_mp(total_mag: int, level: int) -> int:
    """blueprint 02 section 2.1: Mag * (4000 + level*5)/1000."""
    return _idiv(total_mag * (CONFIG["MAG_MIN_BASE"] + level * CONFIG["RENEWAL_HP_LEVEL"]), CONFIG["RENEWAL_HP_DIV"])


def legacy_weight(total_str: int, level: int) -> int:
    """Parametabox.cpp:3641."""
    return total_str * CONFIG["LEGACY_WEIGHT_STR"] + level * CONFIG["LEGACY_WEIGHT_LEVEL"]


def legacy_hit_rate(hnd: int, level: int, enemy_level: int) -> int:
    rank = hnd * CONFIG["PHYS_RANK_MULT"] + level
    raw = _idiv((CONFIG["HIT_BASE"] + rank * CONFIG["HIT_RANK_SCALE"]) * (10000 + (level - enemy_level) * CONFIG["HIT_DODGE_LEVEL_SCALE"]), 10000)
    return min(CONFIG["HIT_CAP"], raw)


def legacy_dodge_rate(dex: int, level: int, enemy_level: int) -> int:
    rank = dex * CONFIG["PHYS_RANK_MULT"] + level
    raw = _idiv(rank * CONFIG["DODGE_RANK_SCALE"] * (10000 + (level - enemy_level) * CONFIG["HIT_DODGE_LEVEL_SCALE"]), 10000)
    return min(CONFIG["DODGE_CAP"], raw)


def legacy_enchant_reduction(grade: int, element_a: int) -> int:
    """Server-Attack.cpp: enchantac = (siEnchantDef * monster_ElementA)/100 (integer)."""
    grade = max(0, min(10, grade))
    return _idiv(CONFIG["ENCHANT_DEF"][grade] * element_a, CONFIG["ENCHANT_ELEMENT_DIV"])


# ---------------------------------------------------------------------------
# PROPOSED formulas (audit section 4) -- proposal, untested
# ---------------------------------------------------------------------------

def proposed_gear_flat_def(level: int) -> float:
    """section 4.1: one armor + one helmet + one boots + one belt at item level == Lv."""
    c = CONFIG
    return (
        c["PROP_GEAR_PIECES_BASE"]
        + c["PROP_GEAR_ARMOR_W"] * level
        + c["PROP_GEAR_HELM_W"] * level
        + c["PROP_GEAR_BOOTS_W"] * level
        + c["PROP_GEAR_BELT_W"] * level
    )


def proposed_natural_def(total_vit: int, level: int) -> int:
    """section 4.3 item 4: floor(VIT * (0.15 + 0.01*(Lv-1))) + 5*floor(Lv/10)."""
    ratio = CONFIG["PROP_VIT_DEF_BASE"] + CONFIG["PROP_VIT_DEF_PER_LEVEL"] * (level - 1)
    return int(total_vit * ratio) + milestone(level)


def proposed_str_damage(total_str: int, level: int) -> int:
    ratio = CONFIG["PROP_STR_BASE"] + CONFIG["PROP_STAT_PER_LEVEL"] * (level - 1)
    return int(total_str * ratio) + milestone(level)


def proposed_mag_damage(total_mag: int, level: int) -> int:
    ratio = CONFIG["PROP_MAG_BASE"] + CONFIG["PROP_STAT_PER_LEVEL"] * (level - 1)
    return int(total_mag * ratio) + milestone(level)


def proposed_hp(total_vit: int, level: int) -> int:
    return int(total_vit * (CONFIG["PROP_HP_BASE"] + CONFIG["PROP_HP_PER_LEVEL"] * level))


def proposed_wis_def(total_wis: int, level: int) -> float:
    return (total_wis + level) * CONFIG["PROP_WIS_DEF_MULT"]


def proposed_mitigation(def_value: float, level: int) -> int:
    """section 4.6: DEF*10000 / (DEF + 700 + Level*50). 1/10000 units."""
    denom = def_value + CONFIG["PROP_DEF_DIVISOR_BASE"] + level * CONFIG["PROP_DEF_LEVEL_SCALE"]
    if denom <= 0:
        return 0
    return int(def_value * 10000 / denom)


def proposed_enchant_reduction_pct(grade: int) -> float:
    """section 4.1 item 2: direct % reduction, 6% per grade."""
    grade = max(0, min(10, grade))
    return CONFIG["PROP_ENCHANT_PCT_PER_GRADE"] * grade


def proposed_weight(total_str: int, level: int) -> int:
    c = CONFIG
    return (
        c["PROP_WEIGHT_BASE"]
        + total_str * c["PROP_WEIGHT_STR"]
        + level * c["PROP_WEIGHT_LEVEL"]
        + c["PROP_WEIGHT_MILESTONE"] * (level // c["PROP_MILESTONE_STEP"])
    )


def proposed_luk_crit_pct(luk: int) -> float:
    return min(CONFIG["PROP_LUK_CAP_PCT"], luk * CONFIG["PROP_LUK_CRIT_PCT_PER_POINT"])


def proposed_luk_drop_pct(luk: int) -> float:
    return min(CONFIG["PROP_LUK_CAP_PCT"], luk * CONFIG["PROP_LUK_CRIT_PCT_PER_POINT"])
