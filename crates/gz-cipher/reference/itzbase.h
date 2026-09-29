// Shim for the legacy itzbase.h.
//
// Only the platform/typedef layer is replaced: the legacy header drags in
// winsock2.h/windows.h/__int64/STRSAFE, none of which ITZEncrypt uses. The
// typedefs below are copied from itzbase.h lines 30-46 verbatim, and
// ITZEncrypt.cpp / ITZEncrypt.h are used byte-for-byte unmodified.
#ifndef _ITZBASE_H
#define _ITZBASE_H

#include <string.h>

typedef signed int			SI32;
typedef signed short		SI16;
typedef signed char			SI08;

typedef unsigned int		UI32;
typedef unsigned short		UI16;
typedef unsigned char		UI08;

#ifndef BOOL
#define BOOL int
#endif

// SAL annotations + BOOL constants normally supplied by windows.h.
#ifndef IN
#define IN
#endif
#ifndef OUT
#define OUT
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

#endif
