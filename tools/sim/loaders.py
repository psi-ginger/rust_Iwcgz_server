"""T-014 data loaders for the deployed legacy server data files.

All ``GameServer`` data files are **UTF-16 (with BOM) + tab-separated**.
This was verified directly on disk (first bytes ``ff fe``), not assumed:
the same family also appears as plain 8-bit text in the source tree, so the
decoder is chosen by BOM rather than hard-coded.

Layout facts used here:
  * charattackinfoE.txt -- header row ``Char/Name/SortNUM/...`` (27 cols)
  * charetcinfoE.txt    -- header row ``Char/Name/SortLevel/...`` (24 cols)
  * ItemTypeSwordE.txt  -- 30-col positional schema (blueprint 04b section 3a)
  * ItemTypeArmourE.txt -- header row itself names the columns incl. ``AC``

The loaders only read; nothing here writes to the legacy tree.
"""

from __future__ import annotations

import os

# blueprint 04b section 3a -- ItemTypeSword<T>.txt positional schema (30 columns)
SWORD_SCHEMA = [
    "Unique", "Name", "Code", "IconCode", "ArmCode", "ShowList", "FieldImg",
    "AttackDelay", "Weight", "Skill", "MountSkillPoint", "MakepPbt", "IncPbt",
    "Level", "Condition", "Hnd", "Durability", "UseDate", "MinDamage",
    "MaxDamage", "ProductUnit", "Material", "Tool", "Explain", "AttackE",
    "AttackR", "Grade", "CanUseType", "MinNum", "NewMarket",
]


def decode(path: str) -> str:
    raw = open(path, "rb").read()
    if raw[:2] in (b"\xff\xfe", b"\xfe\xff"):
        return raw.decode("utf-16")
    # 8-bit fallback: legacy CP949. errors=replace so one bad byte cannot kill a run.
    return raw.decode("cp949", errors="replace")


def _split(path: str):
    return [ln for ln in decode(path).splitlines()]


def _header_index(lines, first_col):
    for i, ln in enumerate(lines):
        cols = [c.strip() for c in ln.split("\t")]
        if cols and cols[0] == first_col:
            return i
    raise KeyError("no header row starting with %r in %s" % (first_col, first_col))


def _rows_from(lines, idx, width=None):
    header = [c.strip() for c in lines[idx].split("\t")]
    out = []
    for ln in lines[idx + 1:]:
        cols = ln.split("\t")
        if not cols or not cols[0].strip():
            continue
        first = cols[0].strip()
        if first.startswith("<") or first.lower() in ("char", "name", "unique"):
            continue
        if width is not None:
            cols = cols[:width]
        out.append(dict(zip(header, [c.strip() for c in cols])))
    return out


def load_char_attack(path: str):
    lines = _split(path)
    idx = _header_index(lines, "Char")
    return _rows_from(lines, idx)


def load_char_etc(path: str):
    lines = _split(path)
    # charetcinfoE has a 'Char' header too; the first such header is the data one.
    for i, ln in enumerate(lines):
        if ln.startswith("Char\tName\tSortLevel"):
            return _rows_from(lines, i)
    raise KeyError("no charetcinfo header in %s" % path)


def load_swords(path: str):
    lines = _split(path)
    out = []
    for ln in lines:
        cols = [c.strip() for c in ln.split("\t")]
        if len(cols) < 20 or not cols[0].isdigit():
            continue
        vals = cols[: len(SWORD_SCHEMA)]
        out.append(dict(zip(SWORD_SCHEMA, vals)))
    return out


def load_armour(path: str):
    lines = _split(path)
    idx = None
    for i, ln in enumerate(lines):
        if ln.startswith("Unique\tName\tCode"):
            idx = i
            break
    if idx is None:
        raise KeyError("no armour header in %s" % path)
    return _rows_from(lines, idx)


def default_data_root() -> str:
    """C:\\...\\04_Server_Goonzu\\goonzu-server\\deploy\\GameServer by default.

    Resolved relative to this file so the tool stays portable; overridable via
    --data-root or GOONZU_DATA_ROOT.
    """
    env = os.environ.get("GOONZU_DATA_ROOT")
    if env:
        return env
    here = os.path.dirname(os.path.abspath(__file__))          # tools/sim
    root = os.path.abspath(os.path.join(here, "..", "..", ".."))  # 04_Server_Goonzu
    return os.path.join(root, "goonzu-server", "deploy", "GameServer")
