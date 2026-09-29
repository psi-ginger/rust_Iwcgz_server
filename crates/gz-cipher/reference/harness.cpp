// Ground-truth harness for the legacy ITZ cipher.
//
// Links the byte-for-byte unmodified ITZEncrypt.cpp / ITZEncrypt.h and drives a
// battery of cases through paired cipher instances so encode/decode counter
// positions line up the way a real client/server pair does. Emits JSON that the
// independent Python reference and the Rust port are both diffed against.
#include "ITZEncrypt.h"

#include <string>
#include <vector>
#include <stdio.h>

static const UI08 SENTINEL = 0xAA;
static const size_t DEST_CAP = 70000;

struct Op {
	bool decode;
	bool follow = false;      // decode the previous op's actual output
	std::vector<UI08> msg;
};

struct Case {
	const char *name;
	UI08 key1;
	UI08 key2;
	bool paired;              // decode on a second, identically-keyed instance
	std::vector<Op> ops;
};

static std::vector<UI08> bytes_from_hex(const char *hex) {
	std::vector<UI08> v;
	std::string s;
	for (const char *p = hex; *p; ++p) {
		if (*p != ' ' && *p != '\t') s += *p;
	}
	for (size_t i = 0; i + 1 < s.size(); i += 2) {
		v.push_back((UI08)strtoul(s.substr(i, 2).c_str(), NULL, 16));
	}
	return v;
}

static std::string hex_of(const UI08 *p, size_t n) {
	std::string s;
	char buf[4];
	for (size_t i = 0; i < n; ++i) {
		sprintf(buf, "%02x", p[i]);
		s += buf;
	}
	return s;
}

static UI16 size_of(const std::vector<UI08> &m) {
	if (m.size() < 2) return 0;
	return (UI16)(m[0] | (m[1] << 8));
}

static Case mk(const char *name, UI08 k1, UI08 k2, bool paired = false) {
	Case c;
	c.name = name; c.key1 = k1; c.key2 = k2; c.paired = paired;
	return c;
}

int main() {
	std::vector<Case> cases;

	// --- Group A: passthrough branch, size <= 3. No cipher, but keys still tick.
	{
		Case c = mk("A1_encode_passthrough_size0", 0x00, 0x00);
		Op o; o.decode = false; o.msg = bytes_from_hex("00 00"); c.ops.push_back(o);
		cases.push_back(c);
	}
	{
		Case c = mk("A2_encode_passthrough_size1", 0x00, 0x00);
		Op o; o.decode = false; o.msg = bytes_from_hex("01 00 ab"); c.ops.push_back(o);
		cases.push_back(c);
	}
	{
		Case c = mk("A3_encode_passthrough_size2", 0x00, 0x00);
		Op o; o.decode = false; o.msg = bytes_from_hex("02 00 ab cd"); c.ops.push_back(o);
		cases.push_back(c);
	}
	{
		Case c = mk("A4_encode_passthrough_size3", 0x00, 0x00);
		Op o; o.decode = false; o.msg = bytes_from_hex("03 00 ab cd ef"); c.ops.push_back(o);
		cases.push_back(c);
	}
	{
		// A 3-byte passthrough must still advance the counter, or the next real
		// packet desynchronises the two peers.
		Case c = mk("A5_passthrough_ticks_keys", 0x00, 0x00);
		Op o1; o1.decode = false; o1.msg = bytes_from_hex("03 00 ab cd ef");
		Op o2; o2.decode = false; o2.msg = bytes_from_hex("04 00 00 11");
		c.ops.push_back(o1); c.ops.push_back(o2);
		cases.push_back(c);
	}

	// --- Group B: cipher branch, smallest size through a larger packet, zero key
	{
		Case c = mk("B1_encode_size4", 0x00, 0x00);
		Op o; o.decode = false; o.msg = bytes_from_hex("04 00 00 11"); c.ops.push_back(o);
		cases.push_back(c);
	}
	{
		Case c = mk("B2_encode_size5", 0x00, 0x00);
		Op o; o.decode = false; o.msg = bytes_from_hex("05 00 00 11 22"); c.ops.push_back(o);
		cases.push_back(c);
	}
	{
		Case c = mk("B3_encode_size8", 0x00, 0x00);
		Op o; o.decode = false; o.msg = bytes_from_hex("08 00 00 11 22 33 44 55 66 77");
		c.ops.push_back(o);
		cases.push_back(c);
	}
	{
		Case c = mk("B4_encode_size64", 0x00, 0x00);
		Op o; o.decode = false; o.msg = bytes_from_hex("40 00");
		for (int i = 0; i < 0x3E; ++i) o.msg.push_back((UI08)(i * 7 + 3));
		c.ops.push_back(o);
		cases.push_back(c);
	}

	// --- Group C: the real Encrypt2 link key 0x12,0x34
	{
		Case c = mk("C1_enc2_encode_size8", 0x12, 0x34);
		Op o; o.decode = false; o.msg = bytes_from_hex("08 00 aa bb cc dd ee ff");
		c.ops.push_back(o);
		cases.push_back(c);
	}
	{
		// Bytes past the declared size are outside the frame and are ignored, so
		// this 9-byte buffer declares 8 and must encode like C1.
		Case c = mk("C8_trailing_byte_ignored", 0x12, 0x34);
		Op o; o.decode = false; o.msg = bytes_from_hex("08 00 aa bb cc dd ee ff 99");
		c.ops.push_back(o);
		cases.push_back(c);
	}
	{
		Case c = mk("C2_enc2_encode_size16", 0x12, 0x34);
		Op o; o.decode = false; o.msg = bytes_from_hex("10 00");
		for (int i = 0; i < 14; ++i) o.msg.push_back((UI08)(0xF0 - i));
		c.ops.push_back(o);
		cases.push_back(c);
	}
	{
		// Identical plaintext twice: the counter must change the ciphertext.
		Case c = mk("C3_enc2_counter_advances", 0x12, 0x34);
		Op o1; o1.decode = false; o1.msg = bytes_from_hex("08 00 aa bb cc dd ee ff");
		Op o2; o2.decode = false; o2.msg = bytes_from_hex("08 00 aa bb cc dd ee ff");
		c.ops.push_back(o1); c.ops.push_back(o2);
		cases.push_back(c);
	}
	{
		// Peer round trip: encode on one instance, decode on a second identically
		// keyed instance so both counters sit at the same position.
		Case c = mk("C4_enc2_roundtrip", 0x12, 0x34, true);
		Op o1; o1.decode = false; o1.msg = bytes_from_hex("08 00 aa bb cc dd ee ff");
		Op o2; o2.decode = true;  o2.follow = true;
		c.ops.push_back(o1); c.ops.push_back(o2);
		cases.push_back(c);
	}
	{
		Case c = mk("C5_enc2_roundtrip_size16", 0x12, 0x34, true);
		Op o1; o1.decode = false; o1.msg = bytes_from_hex("10 00");
		for (int i = 0; i < 14; ++i) o1.msg.push_back((UI08)(0xF0 - i));
		Op o2; o2.decode = true; o2.follow = true;
		c.ops.push_back(o1); c.ops.push_back(o2);
		cases.push_back(c);
	}
	{
		Case c = mk("C6_enc2_roundtrip_size64", 0x12, 0x34, true);
		Op o1; o1.decode = false; o1.msg = bytes_from_hex("40 00");
		for (int i = 0; i < 0x3E; ++i) o1.msg.push_back((UI08)(i * 7 + 3));
		Op o2; o2.decode = true; o2.follow = true;
		c.ops.push_back(o1); c.ops.push_back(o2);
		cases.push_back(c);
	}
	{
		// Multi-packet round trip: three packets through both peers in order, so
		// the counter has to stay in step across a whole burst.
		Case c = mk("C7_enc2_roundtrip_burst", 0x12, 0x34, true);
		Op e1; e1.decode = false; e1.msg = bytes_from_hex("04 00 00 11");
		Op d1; d1.decode = true;  d1.follow = true;
		Op e2; e2.decode = false; e2.msg = bytes_from_hex("07 00 11 22 33 44 55 66");
		Op d2; d2.decode = true;  d2.follow = true;
		Op e3; e3.decode = false; e3.msg = bytes_from_hex("40 00");
		for (int i = 0; i < 0x3E; ++i) e3.msg.push_back((UI08)(i * 13 + 1));
		Op d3; d3.decode = true;  d3.follow = true;
		c.ops.push_back(e1); c.ops.push_back(d1);
		c.ops.push_back(e2); c.ops.push_back(d2);
		c.ops.push_back(e3); c.ops.push_back(d3);
		cases.push_back(c);
	}

	// --- Group D: key counter wraparound
	{
		Case c = mk("D1_encode_wrap_ff", 0xFF, 0xFF);
		Op o; o.decode = false; o.msg = bytes_from_hex("06 00 01 02 03 04 05");
		c.ops.push_back(o);
		cases.push_back(c);
	}
	{
		Case c = mk("D2_encode_wrap_across_calls", 0xFE, 0xFF);
		Op o1; o1.decode = false; o1.msg = bytes_from_hex("05 00 10 20 30 40");
		Op o2; o2.decode = false; o2.msg = bytes_from_hex("05 00 10 20 30 40");
		c.ops.push_back(o1); c.ops.push_back(o2);
		cases.push_back(c);
	}
	{
		Case c = mk("D3_roundtrip_wrap", 0xFF, 0xFE, true);
		Op o1; o1.decode = false; o1.msg = bytes_from_hex("06 00 01 02 03 04 05");
		Op o2; o2.decode = true;  o2.msg = o1.msg;
		c.ops.push_back(o1); c.ops.push_back(o2);
		cases.push_back(c);
	}

	// --- Group E: decode edge cases
	{
		Case c = mk("E1_decode_valid", 0x00, 0x00, true);
		Op o1; o1.decode = false; o1.msg = bytes_from_hex("04 00 00 11");
		Op o2; o2.decode = true;  o2.follow = true;
		c.ops.push_back(o1); c.ops.push_back(o2);
		cases.push_back(c);
	}
	{
		// Single flipped bit in the payload: the one-byte integrity check catches
		// almost all corruption but not all of it.
		Case c = mk("E2_decode_corrupt_payload", 0x00, 0x00);
		Op o; o.decode = true; o.msg = bytes_from_hex("04 00 00 12 35");
		c.ops.push_back(o);
		cases.push_back(c);
	}
	{
		// size > PACKET_MAX_SIZE (65000) is rejected outright. Only the two size
		// bytes are supplied: the legacy returns before touching the body, so if
		// that ever stopped being true this harness would read out of bounds.
		Case c = mk("E3_decode_oversize", 0x00, 0x00);
		Op o; o.decode = true; o.msg = bytes_from_hex("ff ff");
		c.ops.push_back(o);
		cases.push_back(c);
	}
	{
		Case c = mk("E4_decode_passthrough_size3", 0x00, 0x00);
		Op o; o.decode = true; o.msg = bytes_from_hex("03 00 ab cd ef");
		c.ops.push_back(o);
		cases.push_back(c);
	}
	{
		Case c = mk("E5_decode_passthrough_size0", 0x00, 0x00);
		Op o; o.decode = true; o.msg = bytes_from_hex("00 00");
		c.ops.push_back(o);
		cases.push_back(c);
	}
	{
		// Wrong key: the decode still writes output but must reject the packet.
		Case c = mk("E6_decode_wrong_key", 0x99, 0x99);
		Op o; o.decode = true; o.msg = bytes_from_hex("04 00 00 12 34");
		c.ops.push_back(o);
		cases.push_back(c);
	}
	{
		// Corrupt the integrity byte itself; the check is byte 2 vs the last
		// plaintext byte, so this must reject.
		Case c = mk("E7_decode_corrupt_checksum_slot", 0x00, 0x00);
		Op o; o.decode = true; o.msg = bytes_from_hex("05 00 ff 00 11 22");
		c.ops.push_back(o);
		cases.push_back(c);
	}

	printf("{\n  \"cases\": [\n");
	for (size_t ci = 0; ci < cases.size(); ++ci) {
		Case &c = cases[ci];
		// Paired cases decode on a second instance keyed identically, modelling
		// the two ends of a link whose counters advance in lockstep.
		ITZEncrypt tx, rx;
		tx.SetKey(c.key1, c.key2);
		rx.SetKey(c.key1, c.key2);
		ITZEncrypt *enc = &tx;

		printf("    {\n      \"name\": \"%s\",\n      \"key1\": %u, \"key2\": %u,\n      \"ops\": [\n",
		       c.name, (unsigned)c.key1, (unsigned)c.key2);
		std::vector<UI08> last_out;
		for (size_t oi = 0; oi < c.ops.size(); ++oi) {
			Op &op = c.ops[oi];
			// A `follow` op consumes the bytes the previous op actually produced,
			// so round trips exercise the real ciphertext.
			std::vector<UI08> used = op.follow ? last_out : op.msg;
			UI16 sz = size_of(used);
			std::vector<UI08> dest(DEST_CAP, SENTINEL);
			// Decode mutates its input buffer, so drive it on a scratch copy and
			// report the buffer afterwards.
			std::vector<UI08> in = used;
			BOOL ok = TRUE;
			if (op.decode) {
				enc = &rx;
				ok = enc->Decode(&in[0], &dest[0]);
			} else {
				enc = &tx;
				enc->Encode(&used[0], &dest[0]);
			}
			printf("        {\"op\": \"%s\", \"follow\": %s, \"in\": \"%s\", ",
			       op.decode ? "decode" : "encode",
			       op.follow ? "true" : "false",
			       hex_of(used.empty() ? NULL : &used[0], used.size()).c_str());
			if (op.decode) {
				printf("\"ok\": %s, ", ok == TRUE ? "true" : "false");
			}
			// A frame rejected on its size returns before writing anything, so
			// report that rather than dumping the caller's untouched buffer.
			bool rejected_oversize = op.decode && sz > PACKET_MAX_SIZE;
			if (rejected_oversize) {
				printf("\"out\": \"\", \"rejected_oversize\": true, \"overrun\": false, ");
			} else {
				printf("\"out\": \"%s\", ", hex_of(&dest[0], sz).c_str());
			}
			// Anything written past `size` would be an overrun in our harness.
			bool overrun = false;
			for (size_t i = sz; i < sz + 32 && i < DEST_CAP; ++i) {
				if (dest[i] != SENTINEL) { overrun = true; break; }
			}
			printf("\"overrun\": %s, ", overrun ? "true" : "false");
			if (op.decode) {
				printf("\"in_after\": \"%s\", ", hex_of(&in[0], in.size()).c_str());
			}
			printf("\"keys_after\": [%u, %u]}",
			       (unsigned)enc->GetKey1(), (unsigned)enc->GetKey2());
			last_out.assign(dest.begin(), dest.begin() + sz);
			if (oi + 1 < c.ops.size()) printf(",");
			printf("\n");
		}
		printf("      ]\n    }");
		if (ci + 1 < cases.size()) printf(",");
		printf("\n");
	}
	printf("  ]\n}\n");
	return 0;
}
