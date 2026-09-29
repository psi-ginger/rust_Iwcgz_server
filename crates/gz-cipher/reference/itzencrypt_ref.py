"""Independent reference implementation of the legacy ITZ link cipher.

Translated line-by-line from
`NetworkLib\\ITZNetLib\\ITZEncrypt.cpp` (115 lines) without reference to the Rust
port, and used to cross-check the ground truth that the byte-for-byte legacy
C++ harness produces. Agreement between the two, and then the Rust port, is
what makes the test vectors trustworthy.

Run from the directory holding `ground_truth.json`:

    python itzencrypt_ref.py            # cross-check against the C++ harness
    python itzencrypt_ref.py --emit-rs  # regenerate the Rust vector table
"""

from __future__ import annotations

import argparse
import json
import sys

# itzbase.h line 45.
PACKET_MAX_SIZE = 65000


class ITZEncrypt:
    """Port of `ITZEncrypt`. All byte arithmetic wraps at 256 like `UI08`."""

    def __init__(self) -> None:
        # The legacy constructor is empty, so these are indeterminate until
        # SetKey. Zero is the one defined choice; see the crate README.
        self.m_ucKey1 = 0
        self.m_ucKey2 = 0

    def SetKey(self, key1: int, key2: int) -> None:
        self.m_ucKey1 = key1 & 0xFF
        self.m_ucKey2 = key2 & 0xFF

    def GetKey1(self) -> int:
        return self.m_ucKey1

    def GetKey2(self) -> int:
        return self.m_ucKey2

    def Encode(self, src: bytes) -> bytes:
        """Line 30. Returns the `size` bytes the legacy call writes."""
        self.m_ucKey1 = (self.m_ucKey1 + 1) & 0xFF
        self.m_ucKey2 = (self.m_ucKey2 + 1) & 0xFF

        local_usSize = src[0] | (src[1] << 8)

        if local_usSize <= 3:
            return bytes(src[:local_usSize])

        dest = bytearray(local_usSize)

        # Line 44: the integrity byte is the last plaintext byte plus key1.
        dest[2] = (src[local_usSize - 1] + self.m_ucKey1) & 0xFF

        local_cProcessKey1 = self.m_ucKey1
        local_cProcessKey2 = self.m_ucKey2

        dest[0] = local_usSize & 0xFF
        dest[1] = (local_usSize >> 8) & 0xFF

        # pDest += 2 then ++pDest per iteration, so the first write is dest[3].
        d = 2
        for s in range(3, local_usSize):
            d += 1
            local_cProcessKey1 = local_cProcessKey1 ^ src[s]
            dest[d] = (local_cProcessKey1 + local_cProcessKey2) & 0xFF
            local_cProcessKey2 = (local_cProcessKey2 + src[s]) & 0xFF

        return bytes(dest)

    def Decode(self, src: bytes, dest_fill: bytes | None = None):
        """Line 69. Returns (written_bytes, accepted).

        `dest_fill` models the caller's destination buffer. The legacy loop
        starts writing at dest[3], so dest[2] keeps whatever the caller had;
        the accepted flag and the bytes below are unaffected by that byte.
        """
        self.m_ucKey1 = (self.m_ucKey1 + 1) & 0xFF
        self.m_ucKey2 = (self.m_ucKey2 + 1) & 0xFF

        local_usSize = src[0] | (src[1] << 8)

        if local_usSize > PACKET_MAX_SIZE:
            return None, False

        if local_usSize <= 3:
            return bytes(src[:local_usSize]), True

        # Line 85 rewrites the caller's source buffer in place.
        src = bytearray(src)
        src[2] = (src[2] - self.m_ucKey1) & 0xFF

        local_cProcessKey1 = self.m_ucKey1
        local_cProcessKey2 = self.m_ucKey2

        dest = bytearray(local_usSize)
        if dest_fill is not None:
            for i in range(min(len(dest_fill), local_usSize)):
                dest[i] = dest_fill[i]

        dest[0] = local_usSize & 0xFF
        dest[1] = (local_usSize >> 8) & 0xFF

        d = 2
        for s in range(3, local_usSize):
            d += 1
            t = (src[s] - local_cProcessKey2) & 0xFF
            t = t ^ local_cProcessKey1
            dest[d] = t
            local_cProcessKey1 = local_cProcessKey1 ^ t
            local_cProcessKey2 = (local_cProcessKey2 + t) & 0xFF

        # Line 111: the integrity byte must equal the last plaintext byte.
        return bytes(dest), (src[2] == dest[d])


def replay(case: dict, sentinel: bytes) -> list[dict]:
    """Re-run one harness case through the reference, returning op results."""
    tx = ITZEncrypt()
    tx.SetKey(case["key1"], case["key2"])
    rx = ITZEncrypt()
    rx.SetKey(case["key1"], case["key2"])

    results = []
    last_out = b""
    for op in case["ops"]:
        used = last_out if op["follow"] else bytes.fromhex(op["in"])
        if op["op"] == "decode":
            out, ok = rx.Decode(used, dest_fill=sentinel)
            enc = rx
        else:
            out, ok = tx.Encode(used), None
            enc = tx
        results.append(
            {
                "op": op["op"],
                "in": used.hex(),
                "out": None if out is None else out.hex(),
                "ok": ok,
                "keys_after": [enc.GetKey1(), enc.GetKey2()],
            }
        )
        last_out = out if out is not None else b""
    return results


def cross_check(gt: dict) -> int:
    sentinel = b"\xAA" * 70000
    failures = 0

    def brief(value: str | None) -> str:
        if value is None:
            return "None"
        if len(value) > 40:
            return value[:20] + "..(%d)" % (len(value) // 2) + ".." + value[-12:]
        return value

    for case in gt["cases"]:
        mine = replay(case, sentinel)
        for idx, (exp, got) in enumerate(zip(case["ops"], mine)):
            where = "%s op#%d" % (case["name"], idx)
            if got["in"] != exp["in"]:
                print("FAIL %-46s input mismatch" % where)
                failures += 1
            if got["keys_after"] != exp["keys_after"]:
                print(
                    "FAIL %-46s keys %s != %s"
                    % (where, got["keys_after"], exp["keys_after"])
                )
                failures += 1

            # A rejected oversize frame writes nothing: the legacy call returns
            # before touching the destination, so the C++ side reports no output
            # and the reference reports None.
            declared = bytes.fromhex(got["in"])
            size = declared[0] | (declared[1] << 8)
            rejected = exp["op"] == "decode" and size > PACKET_MAX_SIZE
            if rejected:
                if got["out"] is not None:
                    print("FAIL %-46s oversize must produce no output" % where)
                    failures += 1
                if exp["out"] != "" or not exp.get("rejected_oversize"):
                    print("FAIL %-46s oversize wrote to the destination" % where)
                    failures += 1
            elif got["out"] != exp["out"]:
                print("FAIL %-46s out" % where)
                print("       C++: %s" % brief(exp["out"]))
                print("       py : %s" % brief(got["out"]))
                failures += 1

            if "ok" in exp and got["ok"] != exp["ok"]:
                print("FAIL %-46s ok %s != %s" % (where, got["ok"], exp["ok"]))
                failures += 1
            if exp.get("overrun"):
                print("FAIL %-46s legacy overran its output" % where)
                failures += 1
    print(
        "\n%d cases / %d ops cross-checked, %d mismatches"
        % (
            len(gt["cases"]),
            sum(len(c["ops"]) for c in gt["cases"]),
            failures,
        )
    )
    return failures


def emit_rs(gt: dict, max_hex: int) -> None:
    """Print the Rust vector table consumed by tests/vectors.rs.

    Ops whose buffers exceed `max_hex` bytes are skipped: the 65 KB oversize
    frame is covered by a hand-written test instead, since the legacy decode
    rejects it after reading only the two length bytes.
    """
    print("// @generated by reference/itzencrypt_ref.py --emit-rs -- do not edit")
    print("//")
    print("// Ground truth is the byte-for-byte legacy ITZEncrypt.cpp compiled with")
    print("// g++; this file's values were produced by that harness and independently")
    print("// confirmed by the Python reference above. See reference/README.md.")
    print()
    print("/// One call against a cipher instance, as recorded from the legacy C++.")
    print("pub struct Op {")
    print("    /// `true` for `ITZEncrypt::Decode`, `false` for `Encode`.")
    print("    pub decode: bool,")
    print("    /// `true` when the input is the previous op's output (a round trip).")
    print("    pub follow: bool,")
    print("    /// Hex input, ignored when `follow`.")
    print("    pub input: &'static str,")
    print("    /// Hex bytes the call wrote, the `size` bytes the legacy defines.")
    print("    pub output: &'static str,")
    print("    /// `Decode`'s return value; always `true` for `Encode`.")
    print("    pub accepted: bool,")
    print("}")
    print()
    print("/// A sequence of calls against one pair of instances, `key1`/`key2` initial.")
    print("pub struct Case {")
    print("    pub name: &'static str,")
    print("    pub key1: u8,")
    print("    pub key2: u8,")
    print("    pub ops: &'static [Op],")
    print("}")
    print()
    print("pub const CASES: &[Case] = &[")
    skipped = 0
    for case in gt["cases"]:
        rows = []
        for op in case["ops"]:
            # A call the legacy refused on its size writes nothing, so it has no
            # output to pin. tests/vectors.rs drives that case directly.
            if op.get("rejected_oversize"):
                skipped += 1
                continue
            if len(op["in"]) > max_hex or len(op["out"]) > max_hex:
                skipped += 1
                continue
            rows.append(op)
        if not rows:
            continue
        print("    // %s" % case["name"])
        print("    Case {")
        print('        name: "%s",' % case["name"])
        print("        key1: %d," % case["key1"])
        print("        key2: %d," % case["key2"])
        print("        ops: &[")
        for op in rows:
            print("            Op {")
            print(
                "                decode: %s,"
                % ("true" if op["op"] == "decode" else "false")
            )
            print("                follow: %s," % ("true" if op["follow"] else "false"))
            print('                input: "%s",' % op["in"])
            print('                output: "%s",' % op["out"])
            print(
                "                accepted: %s,"
                % ("true" if op.get("ok", True) else "false")
            )
            print("            },")
        print("        ],")
        print("    },")
    print("];")
    if skipped:
        print("// %d call(s) omitted (no output to pin); see tests/vectors.rs" % skipped)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--gt", default="ground_truth.json")
    ap.add_argument("--emit-rs", action="store_true")
    ap.add_argument(
        "--max-hex",
        type=int,
        default=400,
        help="skip ops whose input or output hex string is longer than this",
    )
    args = ap.parse_args()

    with open(args.gt, encoding="utf-8-sig") as fh:
        gt = json.load(fh)

    if args.emit_rs:
        emit_rs(gt, args.max_hex)
        return 0
    return 1 if cross_check(gt) else 0


if __name__ == "__main__":
    sys.exit(main())
