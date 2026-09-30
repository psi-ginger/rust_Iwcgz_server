#!/usr/bin/env python3
"""Turn the legacy framing ground truth into a Rust table.

`harness.cpp` emits `ground_truth.json` from the byte-for-byte unmodified
ITZRecvBuffer.cpp. This converts that into `../tests/data/vectors.rs`, which is
what `cargo test` replays. The table is generated, never typed: a hand-typed
fixture is a second source of truth, and a disagreement between it and the
legacy would be invisible.

    g++ -O1 -w -o harness.exe harness.cpp ITZRecvBuffer.cpp
    ./harness.exe > ground_truth.json
    python emit_vectors.py
    rustfmt --edition 2024 ../tests/data/vectors.rs
"""
import io
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
TRUTH = os.path.join(HERE, "ground_truth.json")
OUT = os.path.join(HERE, os.pardir, "tests", "data", "vectors.rs")

# Cases where this port deliberately answers differently from the legacy, and why.
#
# `SubHeader`     - the legacy accepts a declared size of 2..=4, which cannot hold
#                   the 5-byte header. BUG-013. This port refuses.
# `RingCapacity`  - a frame larger than the link's receive ring: the legacy wraps
#                   its ring and overwrites the partial frame, delivering a
#                   different frame (or refusing). BUG-014. This port, which has
#                   no ring, reassembles the frame that was actually sent.
#
# Every other case in the table must agree with the legacy exactly.
DIVERGENT = {
    "declared_2": "SubHeader",
    "declared_3": "SubHeader",
    "declared_4": "SubHeader",
    "ring_smaller_than_frame": "RingCapacity",
}

STEP_RE = re.compile(
    r'Step \{ chunk: "(?P<chunk>[^"]*)", completed: (?P<ok>\w+), '
    r"packets_after: (?P<count>-?\d+), packets_after_hex: \[(?P<packets>.*)\] \}"
)


def parse(path):
    """Read the harness's Rust-ish output and return plain data.

    The harness writes JSON-shaped Rust literals rather than real JSON so the
    file stays directly readable in a diff; parsing it back is a few lines and
    avoids pulling the emitter and the parser out of sync.
    """
    text = io.open(path, encoding="utf-8").read()
    # A harness that died mid-run leaves a truncated file, and every step it
    # never wrote would simply be absent from the fixture - a smaller table that
    # still passes `cargo test`. Only a closed document is a complete one.
    if not text.rstrip().endswith("]"):
        raise SystemExit(
            "%s is truncated (no closing `]`): the harness did not finish, "
            "refusing to write a fixture that lost cases" % path
        )
    cases = []
    name = None
    for line in text.splitlines():
        found = re.search(r'name: "([^"]+)"', line)
        if found:
            name = found.group(1)
            cases.append({"name": name, "steps": []})
            continue
        found = STEP_RE.search(line)
        if not found:
            continue
        packets = re.findall(r'"([^"]*)"', found.group("packets"))
        cases[-1]["steps"].append(
            {
                "chunk": found.group("chunk"),
                "completed": found.group("ok") == "true",
                "packets": packets,
            }
        )
    return cases


def check(cases):
    """Refuse to emit anything inconsistent, rather than writing a bad fixture."""
    seen = set()
    for case in cases:
        name = case["name"]
        if name in seen:
            raise SystemExit(f"duplicate case name: {name}")
        seen.add(name)
        if not case["steps"]:
            raise SystemExit(f"{name}: no steps")
        for index, step in enumerate(case["steps"]):
            where = f"{name}[{index}]"
            if step["chunk"] and len(step["chunk"]) % 2:
                raise SystemExit(f"{where}: odd-length hex chunk")
            for packet in step["packets"]:
                if packet == "RING_OVERFLOW":
                    raise SystemExit(f"{where}: ring overflow, harness geometry is wrong")
                if len(packet) % 2:
                    raise SystemExit(f"{where}: odd-length hex packet")
                declared = int.from_bytes(bytes.fromhex(packet[:4]), "little")
                if declared != len(packet) // 2:
                    raise SystemExit(
                        f"{where}: packet declares {declared} but is "
                        f"{len(packet) // 2} bytes"
                    )
            if not step["completed"] and step["packets"]:
                raise SystemExit(f"{where}: refused buffer handed out a frame")
            if not step["completed"] and index != len(case["steps"]) - 1:
                # The harness stops feeding a refused buffer, so a read recorded
                # after one describes a session that no longer exists. This must
                # hold for *every* case, including the last: the harness happens to
                # put its refusal cases last today, which is exactly why an earlier
                # version of this guard exempted `cases[-1]` and silently allowed
                # the bad shape.
                raise SystemExit(f"{where}: steps recorded after a refusal")


def emit(cases):
    out = []
    out.append("// @generated by reference/emit_vectors.py -- do not edit")
    out.append("//")
    out.append("// Ground truth is the byte-for-byte legacy ITZRecvBuffer.cpp compiled with")
    out.append("// g++ and driven the way the IOCP drives it; see reference/README.md. Every")
    out.append("// `completed`/`packets` pair below was recorded from the legacy itself.")
    out.append("")
    out.append("/// One read: the bytes received, and what the legacy buffer did with them.")
    out.append("pub struct Step {")
    out.append("    /// Hex bytes written into the buffer, i.e. the chunk read off the socket.")
    out.append("    pub chunk: &'static str,")
    out.append("    /// `Completion()`'s return value: `false` means the session is disconnected.")
    out.append("    pub completed: bool,")
    out.append("    /// Hex frames `GetFirstPacket()` handed out, in order.")
    out.append("    pub packets: &'static [&'static str],")
    out.append("}")
    out.append("")
    out.append("/// A stream delivered to one buffer, in chunks.")
    out.append("pub struct Case {")
    out.append("    pub name: &'static str,")
    out.append("    pub steps: &'static [Step],")
    out.append("}")
    out.append("")
    out.append("/// Every case, in harness order.")
    out.append("pub static CASES: &[Case] = &[")
    for case in cases:
        out.append("    Case {")
        out.append('        name: "%s",' % case["name"])
        out.append("        steps: &[")
        for step in case["steps"]:
            packets = ", ".join('"%s"' % p for p in step["packets"])
            out.append(
                '            Step { chunk: "%s", completed: %s, packets: &[%s] },'
                % (step["chunk"], "true" if step["completed"] else "false", packets)
            )
        out.append("        ],")
        out.append("    },")
    out.append("];")
    out.append("")
    out.append("/// Cases this port deliberately answers differently from the legacy, and why.")
    out.append("///")
    out.append("/// `SubHeader` - the legacy accepts a declared size of 2..=4, which cannot")
    out.append("/// hold the 5-byte header (BUG-013); this port refuses. `RingCapacity` - a")
    out.append("/// frame larger than the link's receive ring, which the legacy overwrites")
    out.append("/// mid-frame (BUG-014); this port, having no ring, delivers what was sent.")
    out.append("#[derive(Debug, Clone, Copy, PartialEq, Eq)]")
    out.append("pub enum Divergence {")
    out.append("    SubHeader,")
    out.append("    RingCapacity,")
    out.append("}")
    out.append("")
    out.append("/// Every deliberately divergent case, with the reason it diverges.")
    out.append("pub static DIVERGENT: &[(&str, Divergence)] = &[")
    for name in sorted(DIVERGENT):
        out.append('    ("%s", Divergence::%s),' % (name, DIVERGENT[name]))
    out.append("];")
    out.append("")
    return "\n".join(out)


def main():
    cases = parse(TRUTH)
    if not cases:
        raise SystemExit("no cases parsed from %s" % TRUTH)
    check(cases)
    missing = set(DIVERGENT) - {c["name"] for c in cases}
    if missing:
        raise SystemExit("divergent cases absent from the harness: %s" % sorted(missing))
    io.open(OUT, "w", encoding="utf-8", newline="\n").write(emit(cases))
    print(
        "%d cases / %d steps written to %s (%d divergent)"
        % (
            len(cases),
            sum(len(c["steps"]) for c in cases),
            os.path.normpath(OUT),
            len(DIVERGENT),
        )
    )


if __name__ == "__main__":
    sys.exit(main())
