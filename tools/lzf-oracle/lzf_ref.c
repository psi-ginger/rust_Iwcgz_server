/*
 * Differential oracle for the gz-lzf port (M0 T-005 follow-up, 2026-10-02).
 *
 * Compiled against the UNMODIFIED legacy sources so its output is ground truth.
 * Reads a file named on the command line and reports as TEXT on stdout:
 *
 *     got    <decimal>  bytes actually read from the file
 *     maxout <decimal>  output space passed to lzf_compress
 *     n      <decimal>  lzf_compress return value (0 = refused)
 *     wrote  <decimal>  bytes accepted by fwrite (sanity check against n)
 *     hex    <hex>      the compressed bytes ("" when n == 0)
 *     valid  <0|1>      1 if the LEGACY decompressor round-trips hex back to
 *                      the input, i.e. the stream is genuinely valid
 *
 * -- Why a file argument and not stdin --
 *
 * This originally read stdin, and that silently corrupted every measurement.
 * MSVC opens stdin in TEXT mode, and the CRT translates 0x1A (Ctrl-Z, the
 * MS-DOS end-of-file marker) into end-of-file. So `fread` returned early on any
 * input containing a 0x1A byte. Concretely: a 128-byte ramp whose byte 75 was
 * 0x1A arrived as 75 bytes, while an identical-length file of 0x41 arrived as
 * 128. The "legacy stream" being compared against was therefore a compression
 * of the wrong input, which looks exactly like a wire-compatibility bug.
 *
 * `fopen(..., "rb")` has no such translation, so the byte count is what the
 * file actually holds. The `got` line is reported unconditionally so any future
 * transport regression is visible in the output rather than inferred from
 * mismatches.
 *
 * -- Params --
 * Pinned to what the legacy tree resolves to, asserted below:
 *   HLOG=16 VERY_FAST=1 ULTRA_FAST=0 CHECK_INPUT=1
 *   INIT_HTAB=0 LZF_STATE_ARG=0 STRICT_ALIGN=0
 *
 * Build: build_oracle.bat
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "lzf.h"
#include "lzfP.h"

/* Keep the pinned parameters honest: assert what this TU was built with. */
typedef char assert_hlog[(HLOG == 16) ? 1 : -1];
typedef char assert_very_fast[(VERY_FAST == 1) ? 1 : -1];
typedef char assert_ultra_fast[(ULTRA_FAST == 0) ? 1 : -1];
typedef char assert_check_input[(CHECK_INPUT == 1) ? 1 : -1];
typedef char assert_init_htab[(INIT_HTAB == 0) ? 1 : -1];
typedef char assert_state_arg[(LZF_STATE_ARG == 0) ? 1 : -1];
typedef char assert_strict_align[(STRICT_ALIGN == 0) ? 1 : -1];

/* lzf_compress_bound lives in lzfP.c, which is not part of the library build.
 * Reproduced verbatim here: the legacy body is `length + length / 16 + 4 + 3`,
 * the same value as `length + length / 16 + 1 + 5`. */
static unsigned int compress_bound(unsigned int length)
{
    return length + length / 16 + 4 + 3;
}

static int emit_refused(void)
{
    printf("wrote 0\n");
    printf("hex \n");
    printf("valid 0\n");
    fflush(stdout);
    return 0;
}

int main(int argc, char **argv)
{
    FILE *f;
    unsigned char *in;
    unsigned char *out;
    unsigned char *back;
    long file_size;
    size_t len = 0, i, wrote;
    unsigned int n, maxout;
    int m, valid;

    if (argc < 2) {
        fprintf(stderr, "usage: %s <input-file>\n", argv[0]);
        return 2;
    }

    /* Binary mode: no Ctrl-Z translation, no newline rewriting. */
    f = fopen(argv[1], "rb");
    if (!f) {
        fprintf(stderr, "cannot open %s\n", argv[1]);
        return 2;
    }
    if (fseek(f, 0, SEEK_END) != 0) { fclose(f); return 3; }
    file_size = ftell(f);
    if (file_size < 0) { fclose(f); return 3; }
    rewind(f);

    in = (unsigned char *)malloc((size_t)file_size + 1);
    if (!in) { fclose(f); return 2; }

    if (file_size > 0) {
        len = fread(in, 1, (size_t)file_size, f);
        if (ferror(f)) { fclose(f); return 3; }
    }
    fclose(f);

    /* Report unconditionally: a transport that drops bytes must be obvious. */
    printf("got %lu\n", (unsigned long)len);
    if ((long)len != file_size) {
        printf("maxout 0\n");
        printf("n 0\n");
        printf("wrote 0\n");
        printf("hex \n");
        printf("valid 0\n");
        fprintf(stderr, "ORACLE BROKEN: read %lu of %ld bytes\n",
                (unsigned long)len, file_size);
        fflush(stdout);
        return 4;
    }

    maxout = compress_bound((unsigned int)len) + 64;
    out = (unsigned char *)malloc(maxout);
    back = (unsigned char *)malloc(len + 64);
    if (!out || !back) return 2;

    printf("maxout %u\n", maxout);

    n = lzf_compress(in, (unsigned int)len, out, maxout);
    printf("n %u\n", n);

    if (n == 0) return emit_refused();

    /* A correct lzf_compress can never report more than the space it was
     * given. If it does, the call is mismatched and nothing it says can be
     * trusted, so refuse to emit a stream at all. */
    if (n > maxout) {
        emit_refused();
        fprintf(stderr, "ORACLE BROKEN: lzf_compress returned %u > maxout %u\n", n, maxout);
        return 4;
    }

    wrote = fwrite(out, 1, n, stdout);
    fflush(stdout);

    m = (len > 0) ? lzf_decompress(out, n, back, (unsigned int)len + 64) : 0;
    valid = (m > 0 && (unsigned int)m == (unsigned int)len && memcmp(back, in, len) == 0);

    printf("wrote %lu\n", (unsigned long)wrote);
    printf("hex ");
    for (i = 0; i < wrote; i++) printf("%02x", out[i]);
    printf("\n");
    printf("valid %d\n", valid);
    fflush(stdout);

    free(in);
    free(out);
    free(back);
    return 0;
}