// Shim for the legacy itzbase.h, for the ITZRecvBuffer harness.
//
// ITZRecvBuffer only needs the typedef layer, the size constant and a
// CRITICAL_SECTION. The real header drags in winsock2.h / windows.h /
// STRSAFE / __int64, none of which ITZRecvBuffer uses, so this file stands in
// for it. The typedefs below are copied from itzbase.h lines 30-42 verbatim and
// PACKET_MAX_SIZE from lines 44-46; ITZRecvBuffer.cpp / .h are compiled
// byte-for-byte unmodified.
#ifndef _ITZBASE_H
#define _ITZBASE_H

#include <string.h>

typedef signed int			SI32;
typedef signed short		SI16;
typedef signed char			SI08;

typedef unsigned int		UI32;
typedef unsigned short		UI16;
typedef unsigned char		UI08;

#ifndef BYTE
#define BYTE unsigned char
#endif

#ifndef BOOL
#define BOOL int
#endif

#ifndef TRUE
#define TRUE 1
#endif

#ifndef FALSE
#define FALSE 0
#endif

#ifndef PACKET_MAX_SIZE
#define PACKET_MAX_SIZE		65000
#endif

// The legacy takes a real CRITICAL_SECTION because Completion() runs on an IOCP
// completion thread. The harness is single-threaded, so these are no-ops: the
// mutual exclusion is genuinely unnecessary and dropping it removes the
// windows.h dependency without changing a single branch.
typedef int CRITICAL_SECTION;
#define InitializeCriticalSection(cs)	((void)(*(cs) = 0))
#define DeleteCriticalSection(cs)		((void)(cs))
#define EnterCriticalSection(cs)		((void)(cs))
#define LeaveCriticalSection(cs)		((void)(cs))

// Only reachable under headerType == 1, which no call site in the GoonZu tree
// uses. Defined anyway so the legacy compiles, and defined correctly rather than
// as a no-op, so a future harness run with headerType 1 measures something real.
static inline unsigned short ntohs(unsigned short v) {
	return (unsigned short)(((v & 0x00ffu) << 8) | ((v & 0xff00u) >> 8));
}

#endif
