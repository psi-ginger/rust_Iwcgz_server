// Ground-truth harness for the legacy ITZ recv-buffer framing.
//
// Links the byte-for-byte unmodified ITZRecvBuffer.cpp / ITZRecvBuffer.h and
// drives chunked byte streams through it exactly the way the IOCP does: write
// the received bytes at the free span GetRecvParam() hands out, then call
// Completion(). Emits JSON that the Rust port is replayed against, so the
// parser is not merely checked against itself.
//
// What is measured per step:
//   - `Completion()`'s return value, which the IOCP turns into
//     SESSION_STATE_DISCONNECTED on FALSE (ITZNetIOCP.cpp:145-150);
//   - how many whole frames the buffer decided on;
//   - the exact bytes of each frame as GetFirstPacket() hands them out.
//
// The legacy is NOT told which frames are legal: it is fed whatever the case
// says, including the sizes the rewrite deliberately refuses.
//
// Two lifecycle details matter and are reproduced exactly:
//
//   - CreateRecvBuffer() does not set `m_pBeginPtr`; it stays NULL from the
//     constructor. ITZSession::SetSocket calls ClearBuffer() before the session
//     goes CONNECTED (ITZSession.cpp:181), and _closesocket calls it again on
//     close (ITZSession.cpp:305) because sessions are pooled. Skip ClearBuffer
//     and GetFirstPacket() dereferences NULL.
//   - A refused Completion() means the caller disconnects and stops feeding, so
//     the harness stops too. Draining anyway would measure a dead session.
#include "ITZRecvBuffer.h"

#include <stdio.h>
#include <string>
#include <vector>

struct Step {
	std::vector<BYTE> chunk;
	bool completed = false;                          // Completion() return value
	int packets_after = 0;                           // frames handed out since then
	std::vector<std::string> packets_after_hex;      // each frame, as handed out
};

struct Case {
	std::string name;
	SI32 ring_size;
	std::vector<Step> steps;
};

static std::vector<BYTE> from_hex(const std::string &hex) {
	std::vector<BYTE> v;
	for (size_t i = 0; i + 1 < hex.size(); i += 2) {
		v.push_back((BYTE)strtoul(hex.substr(i, 2).c_str(), NULL, 16));
	}
	return v;
}

static std::string hex_of(const BYTE *p, size_t n) {
	std::string s;
	char buf[4];
	for (size_t i = 0; i < n; ++i) {
		sprintf(buf, "%02x", p[i]);
		s += buf;
	}
	return s;
}

/// A frame whose declared size is `declared` - deliberately able to build an
/// illegal size - filled with a pattern so a mismatch points at an offset.
static std::string frame_hex(int declared, size_t total) {
	std::vector<BYTE> v;
	v.push_back((BYTE)(declared & 0xff));
	v.push_back((BYTE)((declared >> 8) & 0xff));
	for (size_t i = 2; i < total; ++i) {
		v.push_back((BYTE)((i * 7 + 3) & 0xff));
	}
	return hex_of(&v[0], v.size());
}

/// A legal frame: 5-byte header, little-endian size, `ucChk` 0, then `usCmd`.
static std::string good_frame_hex(int cmd, size_t payload) {
	std::vector<BYTE> v;
	int size = (int)(5 + payload);
	v.push_back((BYTE)(size & 0xff));
	v.push_back((BYTE)((size >> 8) & 0xff));
	v.push_back(0x00);
	v.push_back((BYTE)(cmd & 0xff));
	v.push_back((BYTE)((cmd >> 8) & 0xff));
	for (size_t i = 0; i < payload; ++i) {
		v.push_back((BYTE)((i * 5 + 1) & 0xff));
	}
	return hex_of(&v[0], v.size());
}

static const SI32 RING = 200000;

static Case named(const std::string &name) {
	Case c;
	c.name = name;
	c.ring_size = RING;
	return c;
}

/// A case whose receive ring is deliberately smaller than the frames it carries.
///
/// This is not a hypothetical geometry: `CreateFactory` passes per-link sizes,
/// and the game client link gets 30000/50000 (`GameProtocol.cpp:25,27`) while the
/// size guard allows 64992. `GetRecvParam` wraps `m_pLastPtr` to `m_pBasePtr`
/// once the ring is full and nothing has been consumed
/// (`ITZRecvBuffer.cpp:78-89`), which overwrites a half-received frame. BUG-014.
static Case named_with_ring(const std::string &name, SI32 ring) {
	Case c;
	c.name = name;
	c.ring_size = ring;
	return c;
}

static Case one(const std::string &name, const std::string &hex) {
	Case c = named(name);
	Step s;
	s.chunk = from_hex(hex);
	c.steps.push_back(s);
	return c;
}

/// The same bytes delivered as three chunks split at `at` and `at2`.
static Case split3(const std::string &name, const std::string &hex, size_t at, size_t at2) {
	Case c = named(name);
	std::vector<BYTE> all = from_hex(hex);
	Step a; a.chunk.assign(all.begin(), all.begin() + at);
	Step b; b.chunk.assign(all.begin() + at, all.begin() + at2);
	Step d; d.chunk.assign(all.begin() + at2, all.end());
	c.steps.push_back(a);
	c.steps.push_back(b);
	c.steps.push_back(d);
	return c;
}

static std::vector<Case> build_cases() {
	std::vector<Case> cases;

	// --- accepted frames, the plain cases ---
	cases.push_back(one("single_frame", good_frame_hex(0x0102, 8)));
	cases.push_back(one("two_frames_one_chunk", good_frame_hex(1, 0) + good_frame_hex(2, 40)));
	cases.push_back(split3("split_across_three_chunks", good_frame_hex(0x0304, 30), 7, 19));

	// One frame delivered a byte at a time: the legacy stops one byte before the
	// end of its ring, and this pins that it still reassembles.
	{
		Case c = named("byte_at_a_time");
		std::vector<BYTE> all = from_hex(good_frame_hex(0x0506, 20));
		for (size_t i = 0; i < all.size(); ++i) {
			Step s;
			s.chunk.push_back(all[i]);
			c.steps.push_back(s);
		}
		cases.push_back(c);
	}

	// The two size bytes and the command split across three reads.
	cases.push_back(split3("header_split_from_body", good_frame_hex(0x0607, 24), 1, 4));

	// Empty read first: must be harmless.
	{
		Case c = named("empty_chunk_then_frame");
		c.steps.push_back(Step());
		Step s;
		s.chunk = from_hex(good_frame_hex(0x0708, 4));
		c.steps.push_back(s);
		cases.push_back(c);
	}

	// A complete frame, then nothing: the exact-boundary case, where the legacy
	// loop ends one byte short of having a header to read.
	{
		Case c = named("frame_then_empty_chunk");
		Step s;
		s.chunk = from_hex(good_frame_hex(9, 12));
		c.steps.push_back(s);
		c.steps.push_back(Step());
		cases.push_back(c);
	}

	// A frame followed by an incomplete one: the partial must be held, not cut.
	cases.push_back(one("frame_then_partial", good_frame_hex(1, 10)
		+ good_frame_hex(2, 50).substr(0, 48)));

	// A header that promises far more than arrives, at both ends of the range.
	// These must be *held*, not refused: the size is legal, the body is late.
	// This is also the cheap form of the upper-bound test - a full 64992-byte
	// body is not recorded here because its hex is 130 KB of fixture. That the
	// frame is also *deliverable* end to end is covered in ../tests/framing.rs.
	cases.push_back(one("declared_5_header_only", frame_hex(5, 5)));
	cases.push_back(one("declared_64992_short_body", frame_hex(64992, 40)));
	cases.push_back(one("declared_300_short_body", frame_hex(300, 40)));

	// --- the low end: what the legacy actually accepts ---
	// Sizes 0 and 1: refused by the legacy's own `< 2` guard.
	cases.push_back(one("declared_0", frame_hex(0, 8)));
	cases.push_back(one("declared_1", frame_hex(1, 8)));
	// Sizes 2, 3 and 4: ACCEPTED by the legacy even though the header is 5 bytes
	// and `usCmd` lives at offsets 3-4. This is the BUG-013 divergence, recorded
	// from the legacy rather than asserted from memory.
	cases.push_back(one("declared_2", frame_hex(2, 8)));
	cases.push_back(one("declared_3", frame_hex(3, 8)));
	cases.push_back(one("declared_4", frame_hex(4, 8)));
	// Size 5: the smallest legal frame.
	cases.push_back(one("declared_5", frame_hex(5, 8)));

	// --- the high end ---
	cases.push_back(one("declared_64993_over_limit", frame_hex(64993, 40)));
	cases.push_back(one("declared_65535", frame_hex(65535, 40)));

	// A legal frame followed by an over-limit header: the good frame must not be
	// reachable, because a refusal is a disconnect and not a skip.
	cases.push_back(one("good_then_over_limit", good_frame_hex(1, 6) + frame_hex(64993, 40)));

	// --- BUG-014: a frame larger than the link's receive ring ---
	//
	// Ring 32, frame declares 40. Read 1 fills the ring: the size is legal and the
	// body is late, so the frame is held. Read 2 wraps `m_pLastPtr` back to
	// `m_pBasePtr` and overwrites the first 8 bytes of the partial frame, so the
	// legacy re-reads a size out of the *second* read's bytes. Those bytes are
	// chosen so the corruption is deterministic and visible: `05 00` becomes a
	// bogus 5-byte "frame", and the next header it finds there (`04 00`) runs past
	// the end of what was received, so the loop stops.
	//
	// gz-net has no ring, so it reassembles the correct 40-byte frame. That is the
	// divergence this case exists to pin, so it must NOT be copied into a shape
	// where both sides are expected to agree.
	{
		Case c = named_with_ring("ring_smaller_than_frame", 32);
		Step a;
		a.chunk = from_hex(frame_hex(40, 40).substr(0, 64));   // first 32 bytes
		Step b;
		b.chunk = from_hex("0500aabbcc040000");                // 8 bytes, wraps
		c.steps.push_back(a);
		c.steps.push_back(b);
		cases.push_back(c);
	}

	return cases;
}

static void run(const Case &c, FILE *out) {
	ITZRecvBuffer buf;
	buf.CreateRecvBuffer(c.ring_size, 0);
	buf.ClearBuffer();

	fprintf(out, "    Case {\n        name: \"%s\",\n        steps: [\n", c.name.c_str());

	for (size_t si = 0; si < c.steps.size(); ++si) {
		const Step &step = c.steps[si];

		BYTE *where = NULL;
		SI32 free_size = 0;
		buf.GetRecvParam(&where, &free_size);

		if ((SI32)step.chunk.size() > free_size) {
			// Unreachable for these cases: it would mean the harness, not the
			// legacy, was wrong about the ring geometry.
			fprintf(out, "        Step { chunk: \"%s\", completed: false, "
						 "packets_after: -1, packets_after_hex: [\"RING_OVERFLOW\"] },\n",
					hex_of(step.chunk.empty() ? NULL : &step.chunk[0], step.chunk.size()).c_str());
			continue;
		}

		if (!step.chunk.empty()) {
			memcpy(where, &step.chunk[0], step.chunk.size());
		}
		bool completed = buf.Completion((SI32)step.chunk.size()) ? true : false;

		std::vector<std::string> frames;
		for (;;) {
			BYTE *p = buf.GetFirstPacket();
			if (p == NULL) break;
			SI32 len = (SI32)(*(UI16 *)p);
			frames.push_back(hex_of(p, (size_t)len));
			buf.RemoveFirstPacket();
		}

		fprintf(out, "        Step { chunk: \"%s\", completed: %s, packets_after: %d, "
					 "packets_after_hex: [",
				hex_of(step.chunk.empty() ? NULL : &step.chunk[0], step.chunk.size()).c_str(),
				completed ? "true" : "false", (int)frames.size());
		for (size_t i = 0; i < frames.size(); ++i) {
			fprintf(out, "%s\"%s\"", i ? ", " : "", frames[i].c_str());
		}
		fprintf(out, "] }%s\n", si + 1 == c.steps.size() ? "" : ",");

		if (!completed) break;   // a refused buffer is a dead session
	}

	fprintf(out, "        ],\n    },\n");
}

int main(void) {
	fprintf(stdout, "[\n");
	std::vector<Case> cases = build_cases();
	for (size_t i = 0; i < cases.size(); ++i) {
		run(cases[i], stdout);
	}
	fprintf(stdout, "]\n");
	return 0;
}
