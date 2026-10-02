#include <emmintrin.h>
#include <stdio.h>
#include <string.h>

typedef __m128i Vector8;
typedef unsigned char uint8;
typedef unsigned int uint32;

static inline Vector8 vector8_broadcast(const uint8 c) { return _mm_set1_epi8((char) c); }
static inline void vector8_load(Vector8 *v, const uint8 *s) { *v = _mm_loadu_si128((const __m128i *) s); }
static inline Vector8 vector8_eq(const Vector8 v1, const Vector8 v2) { return _mm_cmpeq_epi8(v1, v2); }
static inline Vector8 vector8_or(const Vector8 v1, const Vector8 v2) { return _mm_or_si128(v1, v2); }
static inline uint32 vector8_highbit_mask(const Vector8 v) { return (uint32) _mm_movemask_epi8(v); }
static inline int vector8_is_highbit_set(const Vector8 v) { return _mm_movemask_epi8(v) != 0; }

struct node16 { uint8 kind; uint8 count; uint8 chunks[16]; void *children[16]; };

static int search(struct node16 *n, uint8 chunk)
{
	Vector8 spread_chunk, haystack, cmp;
	uint32 bitfield;
	spread_chunk = vector8_broadcast(chunk);
	vector8_load(&haystack, &n->chunks[0]);
	cmp = vector8_eq(spread_chunk, haystack);
	bitfield = vector8_highbit_mask(cmp);
	bitfield &= ((1u << n->count) - 1);
	return bitfield ? __builtin_ctz(bitfield) : -1;
}

static int any(const uint8 *s, uint8 c)
{
	Vector8 a, b;
	vector8_load(&a, s);
	vector8_load(&b, s + 16);
	return vector8_is_highbit_set(vector8_or(vector8_eq(a, vector8_broadcast(c)), vector8_eq(b, vector8_broadcast(c))));
}

int main(void)
{
	struct node16 n;
	uint8 buf[32];
	int bad = 0;
	memset(&n, 0, sizeof n);
	n.count = 16;
	for (int i = 0; i < 16; i++)
		n.chunks[i] = (uint8) (i * 7 + 3);
	for (int i = 0; i < 32; i++)
		buf[i] = (uint8) (i * 5 + 1);
	for (int c = 0; c < 256; c++) {
		int want = -1;
		for (int i = 0; i < 16; i++)
			if (n.chunks[i] == c) { want = i; break; }
		int got = search(&n, (uint8) c);
		if (got != want) { if (bad++ < 5) printf("search %d: got %d want %d\n", c, got, want); }
		int wa = memchr(buf, c, 32) != 0;
		int ga = any(buf, (uint8) c);
		if (wa != ga) { if (bad++ < 10) printf("any %d: got %d want %d\n", c, ga, wa); }
	}
	printf("%s %d\n", bad ? "BAD" : "ok", bad);
	return bad != 0;
}
