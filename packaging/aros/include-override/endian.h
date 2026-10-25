/* SPDX-License-Identifier: MIT */
/* Copyright (c) 2026 John Knipper */

/* <endian.h> as the AROS SDK ships it, plus the byte-order conversion macros
 * glibc and the BSDs put next to it (htobe16 .. le64toh), which portable C in
 * the Rust graph expects there (tree-sitter with HAVE_ENDIAN_H). The SDK
 * header provides the byte-order constants only. A candidate for the core's
 * posixc <endian.h>; searched before the SDK (scripts/aros-cargo.sh). */
#ifndef FERAIL_AROS_ENDIAN_H
#define FERAIL_AROS_ENDIAN_H

#include_next <endian.h>

#ifndef htobe16
#if defined(__BYTE_ORDER__) && __BYTE_ORDER__ == __ORDER_LITTLE_ENDIAN__
#define htobe16(x) __builtin_bswap16(x)
#define htole16(x) (x)
#define be16toh(x) __builtin_bswap16(x)
#define le16toh(x) (x)
#define htobe32(x) __builtin_bswap32(x)
#define htole32(x) (x)
#define be32toh(x) __builtin_bswap32(x)
#define le32toh(x) (x)
#define htobe64(x) __builtin_bswap64(x)
#define htole64(x) (x)
#define be64toh(x) __builtin_bswap64(x)
#define le64toh(x) (x)
#elif defined(__BYTE_ORDER__) && __BYTE_ORDER__ == __ORDER_BIG_ENDIAN__
#define htobe16(x) (x)
#define htole16(x) __builtin_bswap16(x)
#define be16toh(x) (x)
#define le16toh(x) __builtin_bswap16(x)
#define htobe32(x) (x)
#define htole32(x) __builtin_bswap32(x)
#define be32toh(x) (x)
#define le32toh(x) __builtin_bswap32(x)
#define htobe64(x) (x)
#define htole64(x) __builtin_bswap64(x)
#define be64toh(x) (x)
#define le64toh(x) __builtin_bswap64(x)
#else
#error "endian.h shim: unknown byte order"
#endif
#endif /* htobe16 */

#endif /* FERAIL_AROS_ENDIAN_H */
