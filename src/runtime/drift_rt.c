/* Drift runtime. Ownership rule: every function receives owned values and must release or keep them;
   `V*` parameters (var self) are borrowed slots that may be mutated after copy-on-write. */
#define _GNU_SOURCE
#include "drift_rt.h"
#include <stdio.h>
#include <stdlib.h>
#include <stdarg.h>
#include <errno.h>
#include <unistd.h>
#include <fcntl.h>
#include <dirent.h>
#include <time.h>
#include <sys/stat.h>
#include <sys/wait.h>

const char* d_loc_file = "?";
int d_loc_line = 0;
static int d_argc;
static char** d_argv;

#define S(v) ((DStr*)(v).o)
#define L(v) ((DList*)(v).o)
#define M(v) ((DMap*)(v).o)
#define T(v) ((DTuple*)(v).o)
#define A(v) ((DAdt*)(v).o)

static void* xmalloc(size_t n) {
  void* p = malloc(n ? n : 1);
  if (!p) { fputs("drift: out of memory\n", stderr); exit(102); }
  return p;
}
static void* xrealloc(void* p, size_t n) {
  p = realloc(p, n ? n : 1);
  if (!p) { fputs("drift: out of memory\n", stderr); exit(102); }
  return p;
}
static void* newobj(size_t n) { DObj* o = xmalloc(n); o->rc = 1; return o; }
static V mk(int64_t k, void* o) { V v; v.k = k; v.o = (DObj*)o; return v; }

/* ------------------------------------------------------------------ panics */

_Noreturn void d_panic_c(const char* msg) {
  fflush(stdout);
  fprintf(stderr, "panic at %s:%d: %s\n", d_loc_file, d_loc_line, msg);
  fflush(stderr);
  exit(101);
}
static _Noreturn void panicf(const char* fmt, ...) {
  char b[2048];
  va_list ap;
  va_start(ap, fmt);
  vsnprintf(b, sizeof b, fmt, ap);
  va_end(ap);
  d_panic_c(b);
}
_Noreturn void d_panic_v(V msg) { panicf("%.*s", (int)S(msg)->len, S(msg)->p); }
_Noreturn void d_unimpl(const char* what) { panicf("`%s` is not available in this runtime yet", what); }

/* ------------------------------------------------------------------ strings */

static V str_alloc(int64_t k, int64_t n) {
  DStr* s = newobj(sizeof(DStr));
  s->len = n; s->cap = n; s->nchars = -1;
  s->p = xmalloc((size_t)n + 1);
  s->p[n] = 0;
  return mk(k, s);
}
static V str_from(int64_t k, const char* p, int64_t n) {
  V v = str_alloc(k, n);
  if (n) memcpy(S(v)->p, p, (size_t)n);
  return v;
}
V d_str_new(const char* p, int64_t n) { return str_from(K_STR, p, n); }
static V cstr(const char* p) { return d_str_new(p, (int64_t)strlen(p)); }
static V bytes_new(const char* p, int64_t n) { return str_from(K_BYTES, p, n); }
V d_str_const(const char* p, int64_t n) { V v = d_str_new(p, n); v.o->rc = 0; return v; }
V d_bytes_const(const char* p, int64_t n) { V v = bytes_new(p, n); v.o->rc = 0; return v; }

static int64_t str_chars(DStr* s) {
  if (s->nchars < 0) {
    int64_t c = 0;
    for (int64_t i = 0; i < s->len; i++) if (((unsigned char)s->p[i] & 0xC0) != 0x80) c++;
    s->nchars = c;
  }
  return s->nchars;
}
/* byte offset of character index ci (0 <= ci <= nchars) */
static int64_t char_off(DStr* s, int64_t ci) {
  if (str_chars(s) == s->len) return ci;
  int64_t c = 0, i = 0;
  while (i < s->len && c < ci) {
    i++;
    while (i < s->len && ((unsigned char)s->p[i] & 0xC0) == 0x80) i++;
    c++;
  }
  return i;
}
static int64_t chars_before(const char* p, int64_t off) {
  int64_t c = 0;
  for (int64_t i = 0; i < off; i++) if (((unsigned char)p[i] & 0xC0) != 0x80) c++;
  return c;
}
static int utf8_len(unsigned char c) { return c < 0x80 ? 1 : c < 0xE0 ? 2 : c < 0xF0 ? 3 : 4; }
static uint32_t utf8_at(const char* p, int64_t n, int64_t i, int* len) {
  unsigned char c = (unsigned char)p[i];
  int l = utf8_len(c);
  if (i + l > n) l = 1;
  *len = l;
  if (l == 1) return c;
  uint32_t cp = c & (0xFF >> (l + 1));
  for (int k = 1; k < l; k++) cp = (cp << 6) | ((unsigned char)p[i + k] & 0x3F);
  return cp;
}
static int utf8_valid(const char* s, int64_t n) {
  const unsigned char* p = (const unsigned char*)s;
  int64_t i = 0;
  while (i < n) {
    unsigned char c = p[i];
    if (c < 0x80) { i++; continue; }
    int l = c >= 0xF0 && c <= 0xF4 ? 4 : c >= 0xE0 ? 3 : c >= 0xC2 && c < 0xE0 ? 2 : 0;
    if (!l || c > 0xF4 || i + l > n) return 0;
    for (int k = 1; k < l; k++) if ((p[i + k] & 0xC0) != 0x80) return 0;
    uint32_t cp = c & (0xFF >> (l + 1));
    for (int k = 1; k < l; k++) cp = (cp << 6) | (p[i + k] & 0x3F);
    if ((l == 3 && (cp < 0x800 || (cp >= 0xD800 && cp <= 0xDFFF))) || (l == 4 && (cp < 0x10000 || cp > 0x10FFFF))) return 0;
    i += l;
  }
  return 1;
}

void d_buf_init(DBuf* b) { b->cap = 64; b->len = 0; b->p = xmalloc(64); }
static void buf_reserve(DBuf* b, int64_t extra) {
  if (b->len + extra + 1 > b->cap) {
    while (b->len + extra + 1 > b->cap) b->cap *= 2;
    b->p = xrealloc(b->p, (size_t)b->cap);
  }
}
void d_buf_add(DBuf* b, const char* s, int64_t n) {
  buf_reserve(b, n);
  memcpy(b->p + b->len, s, (size_t)n);
  b->len += n;
}
static void buf_cstr(DBuf* b, const char* s) { d_buf_add(b, s, (int64_t)strlen(s)); }
static void buf_ch(DBuf* b, char c) { buf_reserve(b, 1); b->p[b->len++] = c; }
static void buf_fmt(DBuf* b, const char* fmt, ...) {
  char t[64];
  va_list ap;
  va_start(ap, fmt);
  int n = vsnprintf(t, sizeof t, fmt, ap);
  va_end(ap);
  d_buf_add(b, t, n);
}
static V buf_done_kind(DBuf* b, int64_t k) {
  DStr* s = newobj(sizeof(DStr));
  s->p = b->p; s->len = b->len; s->cap = b->cap - 1; s->p[s->len] = 0; s->nchars = -1;
  return mk(k, s);
}
V d_buf_done(DBuf* b) { return buf_done_kind(b, K_STR); }

/* ------------------------------------------------------------------ objects */

static V list_new(int64_t cap) {
  DList* l = newobj(sizeof(DList));
  l->len = 0; l->cap = cap;
  l->items = cap ? xmalloc((size_t)cap * sizeof(V)) : NULL;
  return mk(K_LIST, l);
}
static void lpush(V l, V x) {
  DList* d = L(l);
  if (d->len == d->cap) {
    d->cap = d->cap ? d->cap * 2 : 4;
    d->items = xrealloc(d->items, (size_t)d->cap * sizeof(V));
  }
  d->items[d->len++] = x;
}
V d_list(int64_t n, V* items) {
  V v = list_new(n);
  if (n) memcpy(L(v)->items, items, (size_t)n * sizeof(V));
  L(v)->len = n;
  return v;
}
static void list_unique(V* p) {
  DList* l = L(*p);
  if (l->h.rc == 1) return;
  V c = list_new(l->len);
  for (int64_t i = 0; i < l->len; i++) L(c)->items[i] = d_retain(l->items[i]);
  L(c)->len = l->len;
  d_release(*p);
  *p = c;
}

V d_tuple(int64_t n, V* items) {
  DTuple* t = newobj(sizeof(DTuple) + (size_t)n * sizeof(V));
  t->n = n;
  memcpy(t->items, items, (size_t)n * sizeof(V));
  return mk(K_TUPLE, t);
}
static V tup2(V a, V b) { V x[2] = {a, b}; return d_tuple(2, x); }

V d_adt(DType* t, int tag, int n, V* f) {
  if (n == 0) {
    if (!t->units) t->units = calloc((size_t)t->nv, sizeof(V));
    if (!t->units[tag].o) {
      DAdt* a = newobj(sizeof(DAdt));
      a->h.rc = 0; a->t = t; a->tag = tag; a->n = 0;
      t->units[tag] = mk(K_ADT, a);
    }
    return t->units[tag];
  }
  DAdt* a = newobj(sizeof(DAdt) + (size_t)n * sizeof(V));
  a->t = t; a->tag = tag; a->n = n;
  memcpy(a->f, f, (size_t)n * sizeof(V));
  return mk(K_ADT, a);
}
static void adt_unique(V* p) {
  DAdt* a = A(*p);
  if (a->h.rc == 1) return;
  DAdt* c = newobj(sizeof(DAdt) + (size_t)a->n * sizeof(V));
  c->t = a->t; c->tag = a->tag; c->n = a->n;
  for (int i = 0; i < a->n; i++) c->f[i] = d_retain(a->f[i]);
  d_release(*p);
  *p = mk(K_ADT, c);
}
V d_some(V v) { return d_adt(&d_td_core_Option, 0, 1, &v); }
V d_none(void) { return d_adt(&d_td_core_Option, 1, 0, NULL); }
V d_ok(V v) { return d_adt(&d_td_core_Result, 0, 1, &v); }
V d_err(V e) { return d_adt(&d_td_core_Result, 1, 1, &e); }
static int is_some(V v) { return A(v)->tag == 0; }

V d_field(V obj, int i) { V r = d_retain(A(obj)->f[i]); d_release(obj); return r; }
V d_take0(V v) { V r = d_retain(A(v)->f[0]); d_release(v); return r; }
V d_get(V v, int64_t i) {
  switch (v.k) {
  case K_TUPLE: return T(v)->items[i];
  case K_ADT: return A(v)->f[i];
  case K_LIST: return L(v)->items[i];
  default: d_panic_c("internal: pattern access on a non-composite value");
  }
}
V d_list_rest(V v, int64_t n) {
  DList* l = L(v);
  V r = list_new(l->len - n);
  for (int64_t i = n; i < l->len; i++) lpush(r, d_retain(l->items[i]));
  return r;
}
V* d_mut_field(V* p, int i) { adt_unique(p); return &A(*p)->f[i]; }
V d_with(V base, int n, const int* idx, V* vals) {
  adt_unique(&base);
  for (int k = 0; k < n; k++) { d_release(A(base)->f[idx[k]]); A(base)->f[idx[k]] = vals[k]; }
  return base;
}
V d_task(V v) { DTask* t = newobj(sizeof(DTask)); t->val = v; return mk(K_TASK, t); }
void d_immortal(V v) { if (v.k >= K_STR) v.o->rc = 0; }

V d_fn_new(V (*code)(DFn*, V*), int arity, int nenv, V* env) {
  DFn* f = newobj(sizeof(DFn) + (size_t)nenv * sizeof(V));
  f->code = code; f->arity = arity; f->nenv = nenv;
  if (nenv) memcpy(f->env, env, (size_t)nenv * sizeof(V));
  return mk(K_FN, f);
}
V d_call(V f, int n, V* args) {
  (void)n;
  DFn* fo = (DFn*)f.o;
  V r = fo->code(fo, args);
  d_release(f);
  return r;
}
static V call1(V f, V a) { DFn* fo = (DFn*)f.o; V args[1] = {a}; return fo->code(fo, args); }
static V call2(V f, V a, V b) { DFn* fo = (DFn*)f.o; V args[2] = {a, b}; return fo->code(fo, args); }

/* ------------------------------------------------------------------ maps and sets */

static int64_t map_find(DMap* m, V key, uint64_t h);
static V map_new(int64_t k, int64_t cap) {
  DMap* m = newobj(sizeof(DMap));
  if (cap < 4) cap = 4;
  m->len = 0; m->n = 0; m->cap = cap;
  m->e = xmalloc((size_t)cap * sizeof(DEntry));
  int64_t isz = 8;
  while (isz < cap * 2) isz *= 2;
  m->idx = xmalloc((size_t)isz * sizeof(int64_t));
  memset(m->idx, 0xff, (size_t)isz * sizeof(int64_t));
  m->mask = isz - 1;
  return mk(k, m);
}
static void map_index_insert(DMap* m, int64_t ix) {
  int64_t i = (int64_t)(m->e[ix].hash & (uint64_t)m->mask);
  while (m->idx[i] >= 0) i = (i + 1) & m->mask;
  m->idx[i] = ix;
}
static void map_rebuild(DMap* m, int64_t cap) {
  int64_t j = 0;
  for (int64_t i = 0; i < m->n; i++) if (m->e[i].live) m->e[j++] = m->e[i];
  m->n = j;
  if (cap != m->cap) { m->cap = cap; m->e = xrealloc(m->e, (size_t)cap * sizeof(DEntry)); }
  int64_t isz = 8;
  while (isz < m->cap * 2) isz *= 2;
  free(m->idx);
  m->idx = xmalloc((size_t)isz * sizeof(int64_t));
  memset(m->idx, 0xff, (size_t)isz * sizeof(int64_t));
  m->mask = isz - 1;
  for (int64_t i = 0; i < m->n; i++) map_index_insert(m, i);
}
static int64_t map_find(DMap* m, V key, uint64_t h) {
  int64_t i = (int64_t)(h & (uint64_t)m->mask);
  for (;;) {
    int64_t ix = m->idx[i];
    if (ix < 0) return -1;
    DEntry* e = &m->e[ix];
    if (e->live && e->hash == h && d_eq_b(e->key, key)) return ix;
    i = (i + 1) & m->mask;
  }
}
static int64_t map_add_new(DMap* m, V key, V val, uint64_t h) {
  if (m->n == m->cap) map_rebuild(m, m->len + 1 > m->cap / 2 ? m->cap * 2 : m->cap);
  int64_t ix = m->n++;
  m->e[ix].key = key; m->e[ix].val = val; m->e[ix].hash = h; m->e[ix].live = 1;
  m->len++;
  map_index_insert(m, ix);
  return ix;
}
/* takes key and val */
static void map_put(DMap* m, V key, V val) {
  uint64_t h = d_hash(key);
  int64_t ix = map_find(m, key, h);
  if (ix >= 0) { d_release(m->e[ix].val); m->e[ix].val = val; d_release(key); return; }
  map_add_new(m, key, val, h);
}
/* takes key; returns the value slot */
static V* map_slot(DMap* m, V key, int insert) {
  uint64_t h = d_hash(key);
  int64_t ix = map_find(m, key, h);
  if (ix >= 0) { d_release(key); return &m->e[ix].val; }
  if (!insert) {
    DBuf b; d_buf_init(&b); d_show_into(&b, key, 0); buf_ch(&b, 0);
    panicf("key not found: %s", b.p);
  }
  ix = map_add_new(m, key, D_UNIT, h);
  return &m->e[ix].val;
}
/* borrows key; returns the removed value (owned) or 0 */
static int map_take(DMap* m, V key, V* out) {
  int64_t ix = map_find(m, key, d_hash(key));
  if (ix < 0) return 0;
  d_release(m->e[ix].key);
  *out = m->e[ix].val;
  m->e[ix].live = 0;
  m->len--;
  return 1;
}
static void map_compact(DMap* m) { if (m->len != m->n) map_rebuild(m, m->cap); }
static void map_unique(V* p) {
  DMap* m = M(*p);
  if (m->h.rc == 1) return;
  V c = map_new(p->k, m->len);
  for (int64_t i = 0; i < m->n; i++)
    if (m->e[i].live) map_add_new(M(c), d_retain(m->e[i].key), d_retain(m->e[i].val), m->e[i].hash);
  d_release(*p);
  *p = c;
}
V d_map(int64_t n, V* kv) {
  V m = map_new(K_MAP, n);
  for (int64_t i = 0; i < n; i++) map_put(M(m), kv[2 * i], kv[2 * i + 1]);
  return m;
}

/* ------------------------------------------------------------------ free */

void d_free(V v) {
  switch (v.k) {
  case K_STR: case K_BYTES: free(S(v)->p); break;
  case K_LIST: { DList* l = L(v); for (int64_t i = 0; i < l->len; i++) d_release(l->items[i]); free(l->items); break; }
  case K_MAP: case K_SET: {
    DMap* m = M(v);
    for (int64_t i = 0; i < m->n; i++) if (m->e[i].live) { d_release(m->e[i].key); d_release(m->e[i].val); }
    free(m->e); free(m->idx);
    break;
  }
  case K_TUPLE: { DTuple* t = T(v); for (int64_t i = 0; i < t->n; i++) d_release(t->items[i]); break; }
  case K_ADT: { DAdt* a = A(v); for (int i = 0; i < a->n; i++) d_release(a->f[i]); break; }
  case K_FN: { DFn* f = (DFn*)v.o; for (int i = 0; i < f->nenv; i++) d_release(f->env[i]); break; }
  case K_HANDLE: { DHandle* h = (DHandle*)v.o; if (h->fd >= 0) close(h->fd); free(h->path); break; }
  case K_TASK: d_release(((DTask*)v.o)->val); break;
  case K_CHAN: { DChan* c = (DChan*)v.o; d_release(mk(K_LIST, c->q)); break; }
  default: break;
  }
  free(v.o);
}

/* ------------------------------------------------------------------ equality, order, hash */

static int cmpi(int64_t x, int64_t y) { return (x > y) - (x < y); }
static int64_t fkey(double f) { int64_t i; memcpy(&i, &f, 8); return i < 0 ? (i ^ INT64_MAX) : i; }

int d_eq_b(V a, V b) {
  if (a.k != b.k) return 0;
  switch (a.k) {
  case K_UNIT: return 1;
  case K_BOOL: case K_INT: return a.i == b.i;
  case K_FLOAT: return a.f == b.f;
  case K_STR: case K_BYTES: return S(a)->len == S(b)->len && memcmp(S(a)->p, S(b)->p, (size_t)S(a)->len) == 0;
  case K_LIST: {
    DList *x = L(a), *y = L(b);
    if (x->len != y->len) return 0;
    for (int64_t i = 0; i < x->len; i++) if (!d_eq_b(x->items[i], y->items[i])) return 0;
    return 1;
  }
  case K_TUPLE: {
    DTuple *x = T(a), *y = T(b);
    if (x->n != y->n) return 0;
    for (int64_t i = 0; i < x->n; i++) if (!d_eq_b(x->items[i], y->items[i])) return 0;
    return 1;
  }
  case K_ADT: {
    DAdt *x = A(a), *y = A(b);
    if (x->t != y->t || x->tag != y->tag || x->n != y->n) return 0;
    for (int i = 0; i < x->n; i++) if (!d_eq_b(x->f[i], y->f[i])) return 0;
    return 1;
  }
  case K_MAP: case K_SET: {
    DMap *x = M(a), *y = M(b);
    if (x->len != y->len) return 0;
    for (int64_t i = 0; i < x->n; i++) {
      if (!x->e[i].live) continue;
      int64_t j = map_find(y, x->e[i].key, x->e[i].hash);
      if (j < 0) return 0;
      if (a.k == K_MAP && !d_eq_b(x->e[i].val, y->e[j].val)) return 0;
    }
    return 1;
  }
  case K_RANGE: return ((DRange*)a.o)->lo == ((DRange*)b.o)->lo && ((DRange*)a.o)->hi == ((DRange*)b.o)->hi;
  default: return a.o == b.o;
  }
}

int d_cmp_b(V a, V b) {
  if (a.k != b.k) return cmpi(a.k, b.k);
  switch (a.k) {
  case K_UNIT: return 0;
  case K_BOOL: case K_INT: return cmpi(a.i, b.i);
  case K_FLOAT: return cmpi(fkey(a.f), fkey(b.f));
  case K_STR: case K_BYTES: {
    int64_t n = S(a)->len < S(b)->len ? S(a)->len : S(b)->len;
    int c = memcmp(S(a)->p, S(b)->p, (size_t)n);
    if (c) return c < 0 ? -1 : 1;
    return cmpi(S(a)->len, S(b)->len);
  }
  case K_LIST: {
    DList *x = L(a), *y = L(b);
    int64_t n = x->len < y->len ? x->len : y->len;
    for (int64_t i = 0; i < n; i++) { int c = d_cmp_b(x->items[i], y->items[i]); if (c) return c; }
    return cmpi(x->len, y->len);
  }
  case K_TUPLE: {
    DTuple *x = T(a), *y = T(b);
    int64_t n = x->n < y->n ? x->n : y->n;
    for (int64_t i = 0; i < n; i++) { int c = d_cmp_b(x->items[i], y->items[i]); if (c) return c; }
    return cmpi(x->n, y->n);
  }
  case K_ADT: {
    DAdt *x = A(a), *y = A(b);
    if (x->tag != y->tag) return cmpi(x->tag, y->tag);
    for (int i = 0; i < x->n && i < y->n; i++) { int c = d_cmp_b(x->f[i], y->f[i]); if (c) return c; }
    return 0;
  }
  case K_MAP: case K_SET: {
    DMap *x = M(a), *y = M(b);
    if (x->len != y->len) return cmpi(x->len, y->len);
    map_compact(x); map_compact(y);
    for (int64_t i = 0; i < x->n; i++) {
      int c = d_cmp_b(x->e[i].key, y->e[i].key);
      if (!c && a.k == K_MAP) c = d_cmp_b(x->e[i].val, y->e[i].val);
      if (c) return c;
    }
    return 0;
  }
  case K_RANGE: {
    DRange *x = (DRange*)a.o, *y = (DRange*)b.o;
    return x->lo != y->lo ? cmpi(x->lo, y->lo) : cmpi(x->hi, y->hi);
  }
  default: return cmpi((int64_t)(intptr_t)a.o, (int64_t)(intptr_t)b.o);
  }
}

static uint64_t mix(uint64_t x) {
  x += 0x9e3779b97f4a7c15ULL;
  x = (x ^ (x >> 30)) * 0xbf58476d1ce4e5b9ULL;
  x = (x ^ (x >> 27)) * 0x94d049bb133111ebULL;
  return x ^ (x >> 31);
}
static uint64_t hbytes(const char* p, int64_t n) {
  uint64_t h = 1469598103934665603ULL;
  for (int64_t i = 0; i < n; i++) h = (h ^ (unsigned char)p[i]) * 1099511628211ULL;
  return mix(h);
}
uint64_t d_hash(V v) {
  switch (v.k) {
  case K_UNIT: return 0x1234;
  case K_BOOL: case K_INT: return mix((uint64_t)v.i);
  case K_FLOAT: { double f = v.f == 0 ? 0.0 : v.f; uint64_t u; memcpy(&u, &f, 8); return mix(u ^ 0xf10a7ULL); }
  case K_STR: case K_BYTES: return hbytes(S(v)->p, S(v)->len);
  case K_LIST: { uint64_t h = 0x115; for (int64_t i = 0; i < L(v)->len; i++) h = mix(h + d_hash(L(v)->items[i])); return h; }
  case K_TUPLE: { uint64_t h = 0x7u; for (int64_t i = 0; i < T(v)->n; i++) h = mix(h + d_hash(T(v)->items[i])); return h; }
  case K_ADT: {
    uint64_t h = mix((uint64_t)(uintptr_t)A(v)->t ^ (uint64_t)A(v)->tag);
    for (int i = 0; i < A(v)->n; i++) h = mix(h + d_hash(A(v)->f[i]));
    return h;
  }
  case K_MAP: case K_SET: {
    uint64_t h = 0x5e7;
    DMap* m = M(v);
    for (int64_t i = 0; i < m->n; i++)
      if (m->e[i].live) h += mix(m->e[i].hash ^ (v.k == K_MAP ? d_hash(m->e[i].val) * 31 : 0));
    return h;
  }
  case K_RANGE: return mix((uint64_t)((DRange*)v.o)->lo * 31 + (uint64_t)((DRange*)v.o)->hi);
  default: return mix((uint64_t)(uintptr_t)v.o);
  }
}

/* ------------------------------------------------------------------ show */

static void show_float(DBuf* b, double f) {
  if (isnan(f)) { buf_cstr(b, "NaN"); return; }
  if (isinf(f)) { buf_cstr(b, f > 0 ? "inf" : "-inf"); return; }
  char s[40];
  for (int p = 1; p <= 17; p++) {
    snprintf(s, sizeof s, "%.*g", p, f);
    if (strtod(s, NULL) == f) break;
  }
  buf_cstr(b, s);
  if (!strpbrk(s, ".eEn")) buf_cstr(b, ".0");
}
static void show_quoted(DBuf* b, const char* p, int64_t n, int bytes) {
  if (bytes) buf_ch(b, 'b');
  buf_ch(b, '"');
  for (int64_t i = 0; i < n; i++) {
    unsigned char c = (unsigned char)p[i];
    switch (c) {
    case '"': buf_cstr(b, "\\\""); break;
    case '\\': buf_cstr(b, "\\\\"); break;
    case '\n': buf_cstr(b, "\\n"); break;
    case '\t': buf_cstr(b, "\\t"); break;
    case '\r': buf_cstr(b, "\\r"); break;
    case '{': buf_cstr(b, bytes ? "{" : "\\{"); break;
    default:
      if (c < 0x20 || c == 0x7f || (bytes && c >= 0x80)) {
        if (bytes) buf_fmt(b, "\\x%02x", c); else buf_fmt(b, "\\u{%x}", c);
      } else buf_ch(b, (char)c);
    }
  }
  buf_ch(b, '"');
}
void d_show_into(DBuf* b, V v, int top) {
  switch (v.k) {
  case K_UNIT: buf_cstr(b, "()"); break;
  case K_BOOL: buf_cstr(b, v.i ? "true" : "false"); break;
  case K_INT: buf_fmt(b, "%lld", (long long)v.i); break;
  case K_FLOAT: show_float(b, v.f); break;
  case K_STR: if (top) d_buf_add(b, S(v)->p, S(v)->len); else show_quoted(b, S(v)->p, S(v)->len, 0); break;
  case K_BYTES: show_quoted(b, S(v)->p, S(v)->len, 1); break;
  case K_LIST:
    buf_ch(b, '[');
    for (int64_t i = 0; i < L(v)->len; i++) { if (i) buf_cstr(b, ", "); d_show_into(b, L(v)->items[i], 0); }
    buf_ch(b, ']');
    break;
  case K_MAP: case K_SET: {
    DMap* m = M(v);
    buf_cstr(b, v.k == K_MAP ? "{" : "Set.of([");
    int first = 1;
    for (int64_t i = 0; i < m->n; i++) {
      if (!m->e[i].live) continue;
      if (!first) buf_cstr(b, ", ");
      first = 0;
      d_show_into(b, m->e[i].key, 0);
      if (v.k == K_MAP) { buf_cstr(b, ": "); d_show_into(b, m->e[i].val, 0); }
    }
    buf_cstr(b, v.k == K_MAP ? "}" : "])");
    break;
  }
  case K_TUPLE:
    buf_ch(b, '(');
    for (int64_t i = 0; i < T(v)->n; i++) { if (i) buf_cstr(b, ", "); d_show_into(b, T(v)->items[i], 0); }
    if (T(v)->n == 1) buf_ch(b, ',');
    buf_ch(b, ')');
    break;
  case K_ADT: {
    DAdt* a = A(v);
    const DVariant* var = &a->t->v[a->tag];
    if (a->t->bare) buf_cstr(b, var->name);
    else {
      buf_cstr(b, a->t->name);
      if (a->t->is_enum) { buf_ch(b, '.'); buf_cstr(b, var->name); }
    }
    if (var->fields) {
      if (a->n == 0) { buf_cstr(b, " {}"); break; }
      buf_cstr(b, " { ");
      for (int i = 0; i < a->n; i++) {
        if (i) buf_cstr(b, ", ");
        buf_cstr(b, var->fields[i]);
        buf_cstr(b, ": ");
        d_show_into(b, a->f[i], 0);
      }
      buf_cstr(b, " }");
    } else if (a->n > 0) {
      buf_ch(b, '(');
      for (int i = 0; i < a->n; i++) { if (i) buf_cstr(b, ", "); d_show_into(b, a->f[i], 0); }
      buf_ch(b, ')');
    } else if (!a->t->is_enum) buf_cstr(b, " {}");
    break;
  }
  case K_FN: buf_cstr(b, "<fn>"); break;
  case K_RANGE: buf_fmt(b, "%lld..%lld", (long long)((DRange*)v.o)->lo, (long long)((DRange*)v.o)->hi); break;
  case K_HANDLE: buf_cstr(b, "<file "); buf_cstr(b, ((DHandle*)v.o)->path); buf_ch(b, '>'); break;
  case K_TASK: buf_cstr(b, "<task>"); break;
  default: buf_cstr(b, "<channel>"); break;
  }
}
void d_buf_show(DBuf* b, V v, int top) { d_show_into(b, v, top); d_release(v); }

/* ------------------------------------------------------------------ operators */

static V seq_concat(V a, V b) {
  DStr *x = S(a), *y = S(b);
  int64_t n = x->len + y->len;
  if (x->h.rc == 1) {
    if (n > x->cap) {
      int64_t c = x->cap * 2;
      if (c < n) c = n;
      if (c < 16) c = 16;
      x->p = xrealloc(x->p, (size_t)c + 1);
      x->cap = c;
    }
    memcpy(x->p + x->len, y->p, (size_t)y->len);
    x->len = n; x->p[n] = 0; x->nchars = -1;
    d_release(b);
    return a;
  }
  V r = str_alloc(a.k, n);
  memcpy(S(r)->p, x->p, (size_t)x->len);
  memcpy(S(r)->p + x->len, y->p, (size_t)y->len);
  d_release(a); d_release(b);
  return r;
}
V d_add_slow(V a, V b) {
  if (a.k == K_STR || a.k == K_BYTES) return seq_concat(a, b);
  if (a.k == K_LIST) {
    list_unique(&a);
    for (int64_t i = 0; i < L(b)->len; i++) lpush(a, d_retain(L(b)->items[i]));
    d_release(b);
    return a;
  }
  d_panic_c("internal: `+` on unsupported values");
}

V d_index(V base, V idx) {
  if (base.k == K_LIST) {
    DList* l = L(base);
    if (idx.i < 0 || idx.i >= l->len) panicf("index %lld out of range for a list of length %lld", (long long)idx.i, (long long)l->len);
    V r = d_retain(l->items[idx.i]);
    d_release(base);
    return r;
  }
  if (base.k == K_MAP) {
    V* slot = map_slot(M(base), idx, 0);
    V r = d_retain(*slot);
    d_release(base);
    return r;
  }
  if (base.k == K_BYTES) {
    if (idx.i < 0 || idx.i >= S(base)->len) panicf("index %lld out of range for bytes of length %lld", (long long)idx.i, (long long)S(base)->len);
    V r = d_int((unsigned char)S(base)->p[idx.i]);
    d_release(base);
    return r;
  }
  d_panic_c("internal: indexing an unsupported value");
}
V* d_mut_index(V* p, V key, int insert) {
  if (p->k == K_LIST) {
    list_unique(p);
    DList* l = L(*p);
    if (key.i < 0 || key.i >= l->len) panicf("index %lld out of range for a list of length %lld", (long long)key.i, (long long)l->len);
    return &l->items[key.i];
  }
  if (p->k == K_MAP) { map_unique(p); return map_slot(M(*p), key, insert); }
  d_panic_c("internal: assigning into an unsupported value");
}
V d_range(V lo, V hi, int inclusive) {
  DRange* r = newobj(sizeof(DRange));
  int64_t h = hi.i;
  if (inclusive) { if (h == INT64_MAX) d_panic_c("integer overflow in `..=`"); h++; }
  if (h < lo.i) h = lo.i;
  r->lo = lo.i; r->hi = h;
  return mk(K_RANGE, r);
}
int64_t d_iter_len(V it) {
  switch (it.k) {
  case K_LIST: return L(it)->len;
  case K_RANGE: return ((DRange*)it.o)->hi - ((DRange*)it.o)->lo;
  case K_MAP: case K_SET: map_compact(M(it)); return M(it)->n;
  default: d_panic_c("internal: iterating an unsupported value");
  }
}
V d_iter_at(V it, int64_t i) {
  switch (it.k) {
  case K_LIST: return d_retain(L(it)->items[i]);
  case K_RANGE: return d_int(((DRange*)it.o)->lo + i);
  case K_MAP: return tup2(d_retain(M(it)->e[i].key), d_retain(M(it)->e[i].val));
  default: return d_retain(M(it)->e[i].key);
  }
}

/* ------------------------------------------------------------------ std type descriptors */

static const char* const f_message[] = {"message"};
static const char* const f_ioerror[] = {"message", "path"};
static const char* const f_httperror[] = {"message", "status"};
static const char* const f_request[] = {"method", "path", "query", "headers", "body"};
static const DVariant v_option[] = {{"Some", 1, NULL}, {"None", 0, NULL}};
static const DVariant v_result[] = {{"Ok", 1, NULL}, {"Err", 1, NULL}};
static const DVariant v_error[] = {{NULL, 1, f_message}};
static const DVariant v_ioerror[] = {{NULL, 2, f_ioerror}};
static const DVariant v_mode[] = {{"Read", 0, NULL}, {"Write", 0, NULL}, {"Append", 0, NULL}, {"ReadAppendCreate", 0, NULL}};
static const DVariant v_httperror[] = {{NULL, 2, f_httperror}};
static const DVariant v_request[] = {{NULL, 5, f_request}};
static const DVariant v_jsonerror[] = {{NULL, 1, f_message}};
DType d_td_core_Option = {"Option", 2, v_option, 1, 1, NULL};
DType d_td_core_Result = {"Result", 2, v_result, 1, 1, NULL};
DType d_td_core_Error = {"Error", 1, v_error, 0, 0, NULL};
DType d_td_fs_IoError = {"IoError", 1, v_ioerror, 0, 0, NULL};
DType d_td_fs_Mode = {"Mode", 4, v_mode, 1, 0, NULL};
DType d_td_http_HttpError = {"HttpError", 1, v_httperror, 0, 0, NULL};
DType d_td_http_Request = {"Request", 1, v_request, 0, 0, NULL};
DType d_td_json_JsonError = {"JsonError", 1, v_jsonerror, 0, 0, NULL};

static V mkerror(V msg) { return d_adt(&d_td_core_Error, 0, 1, &msg); }
/* any error value -> core Error; an error struct whose first field is `message` keeps it as the text */
V d_to_error(V e) {
  if (e.k == K_ADT && A(e)->t == &d_td_core_Error) return e;
  DBuf b;
  d_buf_init(&b);
  DAdt* a = e.k == K_ADT ? A(e) : NULL;
  const DVariant* var = a ? &a->t->v[a->tag] : NULL;
  if (a && !a->t->is_enum && var->fields && a->n > 0 && !strcmp(var->fields[0], "message") && a->f[0].k == K_STR) {
    d_show_into(&b, a->f[0], 1);
    if (a->n > 1) {
      buf_cstr(&b, " (");
      for (int i = 1; i < a->n; i++) {
        if (i > 1) buf_cstr(&b, ", ");
        buf_cstr(&b, var->fields[i]);
        buf_cstr(&b, ": ");
        d_show_into(&b, a->f[i], 0);
      }
      buf_ch(&b, ')');
    }
  } else d_show_into(&b, e, 0);
  d_release(e);
  return mkerror(d_buf_done(&b));
}

/* ------------------------------------------------------------------ contracts, asserts */

static char* show_c(V v) { DBuf b; d_buf_init(&b); d_show_into(&b, v, 0); buf_ch(&b, 0); return b.p; }
_Noreturn void d_contract_fail(const char* kind, const char* fn, const char* src) {
  panicf("`%s` of `%s` failed: %s", kind, fn, src);
}
_Noreturn void d_assert_fail(const char* src, V msg) {
  if (msg.k == K_STR) panicf("assertion failed: %.*s\n  condition: %s", (int)S(msg)->len, S(msg)->p, src);
  panicf("assertion failed: %s", src);
}
_Noreturn void d_assert_eq_fail(const char* src, V l, V r, V msg) {
  char *ls = show_c(l), *rs = show_c(r);
  if (msg.k == K_STR) panicf("assertion failed: %.*s\n  condition: %s\n  left:  %s\n  right: %s", (int)S(msg)->len, S(msg)->p, src, ls, rs);
  panicf("assertion failed: %s\n  left:  %s\n  right: %s", src, ls, rs);
}
void d_example_fail(const char* file, int line, const char* src, V l, V r) {
  char *ls = show_c(l), *rs = show_c(r);
  fflush(stdout);
  fprintf(stderr, "example failed at %s:%d: %s\n  left:  %s\n  right: %s\n", file, line, src, ls, rs);
  free(ls); free(rs);
}
void d_example_false(const char* file, int line, const char* src) {
  fflush(stdout);
  fprintf(stderr, "example failed at %s:%d: %s\n", file, line, src);
}

/* ------------------------------------------------------------------ program entry, tests */

void d_init(int argc, char** argv) {
  d_argc = argc;
  d_argv = argv;
  if (!isatty(1)) setvbuf(stdout, NULL, _IOFBF, 1 << 16);
}
static void print_error_value(V e) {
  DBuf b;
  d_buf_init(&b);
  if (e.k == K_ADT && A(e)->t == &d_td_core_Error) d_show_into(&b, A(e)->f[0], 1); else d_show_into(&b, e, 0);
  fflush(stdout);
  fprintf(stderr, "error: %.*s\n", (int)b.len, b.p);
  free(b.p);
}
int d_finish_main(V r, int is_result) {
  int code = 0;
  if (is_result && r.k == K_ADT && A(r)->tag == 1) { print_error_value(A(r)->f[0]); code = 1; }
  d_release(r);
  fflush(stdout);
  return code;
}
static void json_str_out(const char* s) {
  putchar('"');
  for (; *s; s++) {
    if (*s == '"' || *s == '\\') { putchar('\\'); putchar(*s); }
    else if (*s == '\n') fputs("\\n", stdout);
    else if ((unsigned char)*s < 0x20) printf("\\u%04x", *s);
    else putchar(*s);
  }
  putchar('"');
}
int d_run_tests(const DTest* t, int n, int argc, char** argv) {
  int json = 0;
  const char* filter = NULL;
  for (int i = 1; i < argc; i++) {
    if (!strcmp(argv[i], "--json")) json = 1; else filter = argv[i];
  }
  int passed = 0, failed = 0;
  for (int i = 0; i < n; i++) {
    if (filter && !strstr(t[i].name, filter)) continue;
    fflush(stdout);
    fflush(stderr);
    int pfd[2];
    if (pipe(pfd) != 0) { perror("drift: pipe"); return 2; }
    pid_t pid = fork();
    if (pid == 0) {
      close(pfd[0]);
      dup2(pfd[1], 2);
      close(pfd[1]);
      V r = t[i].fn();
      int ok;
      if (t[i].is_example) ok = (int)r.i;
      else {
        ok = !(r.k == K_ADT && A(r)->tag == 1);
        if (!ok) print_error_value(A(r)->f[0]);
      }
      fflush(stdout);
      _exit(ok ? 0 : 1);
    }
    close(pfd[1]);
    DBuf err;
    d_buf_init(&err);
    char chunk[4096];
    ssize_t got;
    while ((got = read(pfd[0], chunk, sizeof chunk)) > 0) d_buf_add(&err, chunk, got);
    close(pfd[0]);
    err.p[err.len] = 0;
    int st = 0;
    waitpid(pid, &st, 0);
    int ok = WIFEXITED(st) && WEXITSTATUS(st) == 0;
    if (!ok && !err.len) {
      if (WIFSIGNALED(st)) snprintf(chunk, sizeof chunk, "killed by signal %d\n", WTERMSIG(st));
      else snprintf(chunk, sizeof chunk, "exited with status %d\n", WEXITSTATUS(st));
      buf_cstr(&err, chunk);
      err.p[err.len] = 0;
    }
    if (ok) passed++; else failed++;
    if (json) {
      printf("{\"test\":");
      json_str_out(t[i].name);
      printf(",\"file\":");
      json_str_out(t[i].file);
      printf(",\"line\":%d,\"ok\":%s", t[i].line, ok ? "true" : "false");
      if (err.len) { printf(",\"output\":"); json_str_out(err.p); }
      printf("}\n");
    } else {
      printf("%s %s (%s:%d)\n", ok ? "ok  " : "FAIL", t[i].name, t[i].file, t[i].line);
      if (err.len) {
        for (char* line = strtok(err.p, "\n"); line; line = strtok(NULL, "\n")) printf("    %s\n", line);
      }
    }
    free(err.p);
    fflush(stdout);
  }
  if (!json) printf("%d passed, %d failed\n", passed, failed);
  fflush(stdout);
  return failed ? 1 : 0;
}

/* ================================================================== natives: core */

V d_n_core_error(V msg) { return mkerror(msg); }
V d_n_core_panic(V msg) { d_panic_v(msg); }
V d_n_core_channel(V cap) {
  if (cap.i < 0) d_panic_c("`requires` of `channel` failed: capacity >= 0");
  DChan* c = newobj(sizeof(DChan));
  V q = list_new(0);
  c->q = L(q);
  c->head = 0;
  V ch = mk(K_CHAN, c);
  return tup2(d_retain(ch), ch);
}
V d_n_core_Sender_send(V s, V item) { DChan* c = (DChan*)s.o; lpush(mk(K_LIST, c->q), item); d_release(s); return D_UNIT; }
V d_n_core_Receiver_recv(V r) {
  DChan* c = (DChan*)r.o;
  V out = d_none();
  if (c->head < c->q->len) { out = d_some(c->q->items[c->head]); c->q->items[c->head] = D_UNIT; c->head++; }
  d_release(r);
  return out;
}
V d_n_core_Task_join(V t) { V r = d_retain(((DTask*)t.o)->val); d_release(t); return r; }

/* Int */
V d_n_core_Int_to_float(V x) { return d_float((double)x.i); }
V d_n_core_Int_abs(V x) { if (x.i == INT64_MIN) d_panic_c("integer overflow in `abs`"); return d_int(x.i < 0 ? -x.i : x.i); }
V d_n_core_Int_min(V x, V y) { return d_int(x.i < y.i ? x.i : y.i); }
V d_n_core_Int_max(V x, V y) { return d_int(x.i > y.i ? x.i : y.i); }
V d_n_core_Int_pow(V x, V e) {
  if (e.i < 0) d_panic_c("negative exponent in `pow`");
  int64_t r = 1, b = x.i, n = e.i;
  while (n) {
    if (n & 1) { if (__builtin_mul_overflow(r, b, &r)) d_panic_c("integer overflow in `pow`"); }
    n >>= 1;
    if (n && __builtin_mul_overflow(b, b, &b)) d_panic_c("integer overflow in `pow`");
  }
  return d_int(r);
}
V d_n_core_Int_to_u8(V x) { return x.i >= 0 && x.i <= 255 ? d_some(x) : d_none(); }
V d_n_core_Int_to_u32(V x) { return x.i >= 0 && x.i <= 4294967295LL ? d_some(x) : d_none(); }
V d_n_core_Int_to_u64(V x) { return x.i >= 0 ? d_some(x) : d_none(); }
V d_n_core_Int_to_i32(V x) { return x.i >= INT32_MIN && x.i <= INT32_MAX ? d_some(x) : d_none(); }
V d_n_core_U8_to_int(V x) { return x; }
V d_n_core_U32_to_int(V x) { return x; }
V d_n_core_I32_to_int(V x) { return x; }
V d_n_core_U64_to_int(V x) { return d_some(x); }

/* Float */
V d_n_core_Float_to_int(V x) {
  if (isnan(x.f) || x.f >= 9223372036854775808.0 || x.f < -9223372036854775808.0) panicf("Float %g does not fit in Int", x.f);
  return d_int((int64_t)x.f);
}
V d_n_core_Float_round(V x) { return d_float(round(x.f)); }
V d_n_core_Float_floor(V x) { return d_float(floor(x.f)); }
V d_n_core_Float_ceil(V x) { return d_float(ceil(x.f)); }
V d_n_core_Float_sqrt(V x) { return d_float(sqrt(x.f)); }
V d_n_core_Float_abs(V x) { return d_float(fabs(x.f)); }
V d_n_core_Float_min(V x, V y) { return d_float(x.f < y.f ? x.f : y.f); }
V d_n_core_Float_max(V x, V y) { return d_float(x.f > y.f ? x.f : y.f); }
V d_n_core_Float_is_nan(V x) { return d_bool(isnan(x.f)); }
V d_n_core_Float_to_f32(V x) { return d_float((double)(float)x.f); }
V d_n_core_F32_to_float(V x) { return x; }

/* Str */
static int is_ws(uint32_t c) { return c == ' ' || (c >= 9 && c <= 13) || c == 0xA0 || c == 0x1680 || (c >= 0x2000 && c <= 0x200A) || c == 0x2028 || c == 0x2029 || c == 0x202F || c == 0x205F || c == 0x3000; }
static int is_alpha(uint32_t c) { return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= 0xC0 && c != 0xD7 && c != 0xF7 && !is_ws(c)); }
static int is_digit(uint32_t c) { return c >= '0' && c <= '9'; }
static V str_all(V s, int (*pred)(uint32_t)) {
  DStr* x = S(s);
  int ok = x->len > 0;
  for (int64_t i = 0; ok && i < x->len;) { int l; uint32_t c = utf8_at(x->p, x->len, i, &l); if (!pred(c)) ok = 0; i += l; }
  d_release(s);
  return d_bool(ok);
}
static int is_alnum(uint32_t c) { return is_alpha(c) || is_digit(c); }
V d_n_core_Str_is_alphanumeric(V s) { return str_all(s, is_alnum); }
V d_n_core_Str_is_alphabetic(V s) { return str_all(s, is_alpha); }
V d_n_core_Str_is_numeric(V s) { return str_all(s, is_digit); }
V d_n_core_Str_is_whitespace(V s) { return str_all(s, is_ws); }
V d_n_core_Str_len(V s) { V r = d_int(str_chars(S(s))); d_release(s); return r; }
V d_n_core_Str_is_empty(V s) { V r = d_bool(S(s)->len == 0); d_release(s); return r; }
V d_n_core_Str_chars(V s) {
  DStr* x = S(s);
  V r = list_new(x->len);
  for (int64_t i = 0; i < x->len;) { int l = utf8_len((unsigned char)x->p[i]); if (i + l > x->len) l = 1; lpush(r, d_str_new(x->p + i, l)); i += l; }
  d_release(s);
  return r;
}
V d_n_core_Str_bytes(V s) { V r = bytes_new(S(s)->p, S(s)->len); d_release(s); return r; }
V d_n_core_Str_to_bytes(V s) { return d_n_core_Str_bytes(s); }
static V str_case(V s, int upper) {
  DStr* x = S(s);
  V r = d_str_new(x->p, x->len);
  unsigned char* p = (unsigned char*)S(r)->p;
  for (int64_t i = 0; i < x->len; i++) {
    if (upper) {
      if (p[i] >= 'a' && p[i] <= 'z') p[i] -= 32;
      else if (p[i] == 0xC3 && i + 1 < x->len && p[i + 1] >= 0xA0 && p[i + 1] <= 0xBE && p[i + 1] != 0xB7) { p[i + 1] -= 0x20; i++; }
    } else {
      if (p[i] >= 'A' && p[i] <= 'Z') p[i] += 32;
      else if (p[i] == 0xC3 && i + 1 < x->len && p[i + 1] >= 0x80 && p[i + 1] <= 0x9E && p[i + 1] != 0x97) { p[i + 1] += 0x20; i++; }
    }
  }
  d_release(s);
  return r;
}
V d_n_core_Str_to_lower(V s) { return str_case(s, 0); }
V d_n_core_Str_to_upper(V s) { return str_case(s, 1); }
static int ascii_ws(char c) { return c == ' ' || (c >= 9 && c <= 13); }
V d_n_core_Str_trim(V s) {
  DStr* x = S(s);
  int64_t a = 0, b = x->len;
  while (a < b && ascii_ws(x->p[a])) a++;
  while (b > a && ascii_ws(x->p[b - 1])) b--;
  if (a == 0 && b == x->len) return s;
  V r = d_str_new(x->p + a, b - a);
  d_release(s);
  return r;
}
V d_n_core_Str_trim_matches(V s, V f) {
  DStr* x = S(s);
  int64_t a = 0, b = x->len;
  while (a < b) {
    int l = utf8_len((unsigned char)x->p[a]);
    if (a + l > b) l = 1;
    V r = call1(f, d_str_new(x->p + a, l));
    if (!r.i) break;
    a += l;
  }
  while (b > a) {
    int64_t st = b - 1;
    while (st > a && ((unsigned char)x->p[st] & 0xC0) == 0x80) st--;
    V r = call1(f, d_str_new(x->p + st, b - st));
    if (!r.i) break;
    b = st;
  }
  V out = d_str_new(x->p + a, b - a);
  d_release(s);
  d_release(f);
  return out;
}
V d_n_core_Str_split(V s, V sep) {
  if (S(sep)->len == 0) { d_release(sep); return d_n_core_Str_chars(s); }
  DStr *x = S(s), *y = S(sep);
  V r = list_new(4);
  int64_t i = 0;
  for (;;) {
    char* hit = memmem(x->p + i, (size_t)(x->len - i), y->p, (size_t)y->len);
    if (!hit) { lpush(r, d_str_new(x->p + i, x->len - i)); break; }
    lpush(r, d_str_new(x->p + i, hit - (x->p + i)));
    i = (hit - x->p) + y->len;
  }
  d_release(s); d_release(sep);
  return r;
}
V d_n_core_Str_split_whitespace(V s) {
  DStr* x = S(s);
  V r = list_new(4);
  int64_t i = 0;
  while (i < x->len) {
    while (i < x->len && ascii_ws(x->p[i])) i++;
    int64_t st = i;
    while (i < x->len && !ascii_ws(x->p[i])) i++;
    if (i > st) lpush(r, d_str_new(x->p + st, i - st));
  }
  d_release(s);
  return r;
}
V d_n_core_Str_lines(V s) {
  DStr* x = S(s);
  V r = list_new(4);
  int64_t i = 0;
  while (i < x->len) {
    int64_t st = i;
    while (i < x->len && x->p[i] != '\n') i++;
    int64_t e = i;
    if (e > st && x->p[e - 1] == '\r') e--;
    lpush(r, d_str_new(x->p + st, e - st));
    i++;
  }
  d_release(s);
  return r;
}
V d_n_core_Str_contains(V s, V part) {
  int ok = memmem(S(s)->p, (size_t)S(s)->len, S(part)->p, (size_t)S(part)->len) != NULL;
  d_release(s); d_release(part);
  return d_bool(ok);
}
V d_n_core_Str_starts_with(V s, V p) {
  int ok = S(p)->len <= S(s)->len && memcmp(S(s)->p, S(p)->p, (size_t)S(p)->len) == 0;
  d_release(s); d_release(p);
  return d_bool(ok);
}
V d_n_core_Str_ends_with(V s, V p) {
  int ok = S(p)->len <= S(s)->len && memcmp(S(s)->p + S(s)->len - S(p)->len, S(p)->p, (size_t)S(p)->len) == 0;
  d_release(s); d_release(p);
  return d_bool(ok);
}
V d_n_core_Str_find(V s, V part) {
  char* hit = memmem(S(s)->p, (size_t)S(s)->len, S(part)->p, (size_t)S(part)->len);
  V r = hit ? d_some(d_int(chars_before(S(s)->p, hit - S(s)->p))) : d_none();
  d_release(s); d_release(part);
  return r;
}
V d_n_core_Str_replace(V s, V from, V to) {
  if (S(from)->len == 0) { d_release(from); d_release(to); return s; }
  DStr *x = S(s), *f = S(from), *t = S(to);
  DBuf b;
  d_buf_init(&b);
  int64_t i = 0;
  for (;;) {
    char* hit = memmem(x->p + i, (size_t)(x->len - i), f->p, (size_t)f->len);
    if (!hit) { d_buf_add(&b, x->p + i, x->len - i); break; }
    d_buf_add(&b, x->p + i, hit - (x->p + i));
    d_buf_add(&b, t->p, t->len);
    i = (hit - x->p) + f->len;
  }
  d_release(s); d_release(from); d_release(to);
  return d_buf_done(&b);
}
V d_n_core_Str_slice(V s, V a, V b) {
  int64_t n = str_chars(S(s));
  if (a.i < 0 || b.i < a.i || b.i > n) panicf("slice %lld..%lld out of range for a string of length %lld", (long long)a.i, (long long)b.i, (long long)n);
  int64_t x = char_off(S(s), a.i), y = char_off(S(s), b.i);
  V r = d_str_new(S(s)->p + x, y - x);
  d_release(s);
  return r;
}
V d_n_core_Str_repeat(V s, V n) {
  if (n.i < 0) d_panic_c("negative count in `repeat`");
  DBuf b;
  d_buf_init(&b);
  for (int64_t i = 0; i < n.i; i++) d_buf_add(&b, S(s)->p, S(s)->len);
  d_release(s);
  return d_buf_done(&b);
}
V d_n_core_Str_parse_int(V s) {
  DStr* x = S(s);
  V r = d_none();
  int64_t i = 0, neg = 0, v = 0;
  if (i < x->len && (x->p[i] == '-' || x->p[i] == '+')) { neg = x->p[i] == '-'; i++; }
  if (i < x->len) {
    int ok = 1;
    for (; i < x->len; i++) {
      if (!is_digit((unsigned char)x->p[i])) { ok = 0; break; }
      if (__builtin_mul_overflow(v, 10, &v) || (neg ? __builtin_sub_overflow(v, x->p[i] - '0', &v) : __builtin_add_overflow(v, x->p[i] - '0', &v))) { ok = 0; break; }
    }
    if (ok) r = d_some(d_int(v));
  }
  d_release(s);
  return r;
}
V d_n_core_Str_parse_float(V s) {
  DStr* x = S(s);
  V r = d_none();
  if (x->len > 0 && !ascii_ws(x->p[0])) {
    char* end;
    double f = strtod(x->p, &end);
    if (end == x->p + x->len) r = d_some(d_float(f));
  }
  d_release(s);
  return r;
}

/* Bytes */
V d_n_core_Bytes_len(V b) { V r = d_int(S(b)->len); d_release(b); return r; }
V d_n_core_Bytes_is_empty(V b) { V r = d_bool(S(b)->len == 0); d_release(b); return r; }
V d_n_core_Bytes_slice(V b, V x, V y) {
  if (x.i < 0 || y.i < x.i || y.i > S(b)->len) panicf("slice %lld..%lld out of range for bytes of length %lld", (long long)x.i, (long long)y.i, (long long)S(b)->len);
  V r = bytes_new(S(b)->p + x.i, y.i - x.i);
  d_release(b);
  return r;
}
V d_n_core_Bytes_to_str(V b) {
  V r = utf8_valid(S(b)->p, S(b)->len) ? d_some(d_str_new(S(b)->p, S(b)->len)) : d_none();
  d_release(b);
  return r;
}
static uint64_t read_le(V b, int64_t at, int n) {
  if (at < 0 || at + n > S(b)->len) panicf("reading %d bytes at %lld is out of range for bytes of length %lld", n, (long long)at, (long long)S(b)->len);
  uint64_t v = 0;
  for (int i = n - 1; i >= 0; i--) v = (v << 8) | (unsigned char)S(b)->p[at + i];
  return v;
}
V d_n_core_Bytes_read_u32_le(V b, V at) { V r = d_int((int64_t)read_le(b, at.i, 4)); d_release(b); return r; }
V d_n_core_Bytes_read_u64_le(V b, V at) {
  uint64_t v = read_le(b, at.i, 8);
  if (v > INT64_MAX) d_panic_c("u64 value does not fit in Int");
  d_release(b);
  return d_int((int64_t)v);
}
static V le_bytes(uint64_t v, int n) { char t[8]; for (int i = 0; i < n; i++) { t[i] = (char)(v & 0xff); v >>= 8; } return bytes_new(t, n); }
V d_n_bytes_u32_le(V v) {
  if (v.i < 0 || v.i >= 4294967296LL) d_panic_c("`requires` of `u32_le` failed: value >= 0 and value < 4294967296");
  return le_bytes((uint64_t)v.i, 4);
}
V d_n_bytes_u64_le(V v) {
  if (v.i < 0) d_panic_c("`requires` of `u64_le` failed: value >= 0");
  return le_bytes((uint64_t)v.i, 8);
}

/* List */
V d_n_core_List_len(V l) { V r = d_int(L(l)->len); d_release(l); return r; }
V d_n_core_List_is_empty(V l) { V r = d_bool(L(l)->len == 0); d_release(l); return r; }
V d_n_core_List_get(V l, V i) {
  V r = i.i >= 0 && i.i < L(l)->len ? d_some(d_retain(L(l)->items[i.i])) : d_none();
  d_release(l);
  return r;
}
V d_n_core_List_first(V l) { return d_n_core_List_get(l, d_int(0)); }
V d_n_core_List_last(V l) { return d_n_core_List_get(l, d_int(L(l)->len - 1)); }
V d_n_core_List_push(V* l, V x) { list_unique(l); lpush(*l, x); return D_UNIT; }
V d_n_core_List_pop(V* l) {
  list_unique(l);
  DList* d = L(*l);
  if (!d->len) return d_none();
  return d_some(d->items[--d->len]);
}
V d_n_core_List_insert(V* l, V i, V x) {
  list_unique(l);
  DList* d = L(*l);
  if (i.i < 0 || i.i > d->len) panicf("insert index %lld out of range for a list of length %lld", (long long)i.i, (long long)d->len);
  lpush(*l, D_UNIT);
  memmove(&d->items[i.i + 1], &d->items[i.i], (size_t)(d->len - 1 - i.i) * sizeof(V));
  d->items[i.i] = x;
  return D_UNIT;
}
V d_n_core_List_remove(V* l, V i) {
  list_unique(l);
  DList* d = L(*l);
  if (i.i < 0 || i.i >= d->len) panicf("remove index %lld out of range for a list of length %lld", (long long)i.i, (long long)d->len);
  V x = d->items[i.i];
  memmove(&d->items[i.i], &d->items[i.i + 1], (size_t)(d->len - 1 - i.i) * sizeof(V));
  d->len--;
  return x;
}
V d_n_core_List_contains(V l, V x) {
  int ok = 0;
  for (int64_t i = 0; i < L(l)->len && !ok; i++) ok = d_eq_b(L(l)->items[i], x);
  d_release(l); d_release(x);
  return d_bool(ok);
}
V d_n_core_List_index_of(V l, V x) {
  V r = d_none();
  for (int64_t i = 0; i < L(l)->len; i++) if (d_eq_b(L(l)->items[i], x)) { r = d_some(d_int(i)); break; }
  d_release(l); d_release(x);
  return r;
}
static V list_range(V l, int64_t a, int64_t b) {
  V r = list_new(b - a);
  for (int64_t i = a; i < b; i++) lpush(r, d_retain(L(l)->items[i]));
  d_release(l);
  return r;
}
V d_n_core_List_slice(V l, V a, V b) {
  if (a.i < 0 || b.i < a.i || b.i > L(l)->len) panicf("slice %lld..%lld out of range for a list of length %lld", (long long)a.i, (long long)b.i, (long long)L(l)->len);
  return list_range(l, a.i, b.i);
}
V d_n_core_List_take(V l, V n) { int64_t k = n.i < 0 ? 0 : n.i > L(l)->len ? L(l)->len : n.i; return list_range(l, 0, k); }
V d_n_core_List_skip(V l, V n) { int64_t k = n.i < 0 ? 0 : n.i > L(l)->len ? L(l)->len : n.i; return list_range(l, k, L(l)->len); }
V d_n_core_List_reversed(V l) {
  V r = list_new(L(l)->len);
  for (int64_t i = L(l)->len - 1; i >= 0; i--) lpush(r, d_retain(L(l)->items[i]));
  d_release(l);
  return r;
}
typedef struct { V key, item; } KV;
static void msort(KV* a, KV* tmp, int64_t n) {
  if (n < 2) return;
  int64_t m = n / 2;
  msort(a, tmp, m);
  msort(a + m, tmp, n - m);
  int64_t i = 0, j = m, k = 0;
  while (i < m && j < n) tmp[k++] = d_cmp_b(a[j].key, a[i].key) < 0 ? a[j++] : a[i++];
  while (i < m) tmp[k++] = a[i++];
  while (j < n) tmp[k++] = a[j++];
  memcpy(a, tmp, (size_t)n * sizeof(KV));
}
static V sort_list(V l, V key) {
  int64_t n = L(l)->len;
  KV* a = xmalloc((size_t)n * sizeof(KV));
  KV* t = xmalloc((size_t)n * sizeof(KV));
  for (int64_t i = 0; i < n; i++) {
    a[i].item = d_retain(L(l)->items[i]);
    a[i].key = key.k == K_FN ? call1(key, d_retain(a[i].item)) : a[i].item;
  }
  msort(a, t, n);
  V r = list_new(n);
  for (int64_t i = 0; i < n; i++) { lpush(r, a[i].item); if (key.k == K_FN) d_release(a[i].key); }
  free(a); free(t);
  d_release(l); d_release(key);
  return r;
}
V d_n_core_List_sorted(V l) { return sort_list(l, D_UNIT); }
V d_n_core_List_sorted_by(V l, V key) { return sort_list(l, key); }
V d_n_core_List_map(V l, V f) {
  V r = list_new(L(l)->len);
  for (int64_t i = 0; i < L(l)->len; i++) lpush(r, call1(f, d_retain(L(l)->items[i])));
  d_release(l); d_release(f);
  return r;
}
V d_n_core_List_par_map(V l, V f) { return d_n_core_List_map(l, f); }
V d_n_core_List_filter(V l, V f) {
  V r = list_new(0);
  for (int64_t i = 0; i < L(l)->len; i++) if (call1(f, d_retain(L(l)->items[i])).i) lpush(r, d_retain(L(l)->items[i]));
  d_release(l); d_release(f);
  return r;
}
V d_n_core_List_flat_map(V l, V f) {
  V r = list_new(0);
  for (int64_t i = 0; i < L(l)->len; i++) {
    V part = call1(f, d_retain(L(l)->items[i]));
    for (int64_t j = 0; j < L(part)->len; j++) lpush(r, d_retain(L(part)->items[j]));
    d_release(part);
  }
  d_release(l); d_release(f);
  return r;
}
V d_n_core_List_fold(V l, V acc, V f) {
  for (int64_t i = 0; i < L(l)->len; i++) acc = call2(f, acc, d_retain(L(l)->items[i]));
  d_release(l); d_release(f);
  return acc;
}
V d_n_core_List_find(V l, V f) {
  V r = d_none();
  for (int64_t i = 0; i < L(l)->len; i++) if (call1(f, d_retain(L(l)->items[i])).i) { r = d_some(d_retain(L(l)->items[i])); break; }
  d_release(l); d_release(f);
  return r;
}
V d_n_core_List_any(V l, V f) {
  int ok = 0;
  for (int64_t i = 0; i < L(l)->len && !ok; i++) ok = (int)call1(f, d_retain(L(l)->items[i])).i;
  d_release(l); d_release(f);
  return d_bool(ok);
}
V d_n_core_List_all(V l, V f) {
  int ok = 1;
  for (int64_t i = 0; i < L(l)->len && ok; i++) ok = (int)call1(f, d_retain(L(l)->items[i])).i;
  d_release(l); d_release(f);
  return d_bool(ok);
}
V d_n_core_List_count(V l, V f) {
  int64_t c = 0;
  for (int64_t i = 0; i < L(l)->len; i++) if (call1(f, d_retain(L(l)->items[i])).i) c++;
  d_release(l); d_release(f);
  return d_int(c);
}
V d_n_core_List_sum(V l) {
  V acc = d_int(0);
  if (L(l)->len && L(l)->items[0].k == K_FLOAT) acc = d_float(0);
  for (int64_t i = 0; i < L(l)->len; i++) acc = d_add(acc, d_retain(L(l)->items[i]));
  d_release(l);
  return acc;
}
static V list_extreme(V l, V key, int want) {
  DList* d = L(l);
  V r = d_none();
  if (d->len) {
    int64_t best = 0;
    V bk = key.k == K_FN ? call1(key, d_retain(d->items[0])) : d->items[0];
    for (int64_t i = 1; i < d->len; i++) {
      V k = key.k == K_FN ? call1(key, d_retain(d->items[i])) : d->items[i];
      if (d_cmp_b(k, bk) * want > 0) {
        if (key.k == K_FN) d_release(bk);
        bk = k; best = i;
      } else if (key.k == K_FN) d_release(k);
    }
    if (key.k == K_FN) d_release(bk);
    r = d_some(d_retain(d->items[best]));
  }
  d_release(l); d_release(key);
  return r;
}
V d_n_core_List_min(V l) { return list_extreme(l, D_UNIT, -1); }
V d_n_core_List_max(V l) { return list_extreme(l, D_UNIT, 1); }
V d_n_core_List_min_by(V l, V k) { return list_extreme(l, k, -1); }
V d_n_core_List_max_by(V l, V k) { return list_extreme(l, k, 1); }
V d_n_core_List_enumerate(V l) {
  V r = list_new(L(l)->len);
  for (int64_t i = 0; i < L(l)->len; i++) lpush(r, tup2(d_int(i), d_retain(L(l)->items[i])));
  d_release(l);
  return r;
}
V d_n_core_List_zip(V a, V b) {
  int64_t n = L(a)->len < L(b)->len ? L(a)->len : L(b)->len;
  V r = list_new(n);
  for (int64_t i = 0; i < n; i++) lpush(r, tup2(d_retain(L(a)->items[i]), d_retain(L(b)->items[i])));
  d_release(a); d_release(b);
  return r;
}
V d_n_core_List_chunks(V l, V size) {
  if (size.i <= 0) d_panic_c("chunk size must be positive");
  V r = list_new(0);
  for (int64_t i = 0; i < L(l)->len; i += size.i) {
    int64_t e = i + size.i > L(l)->len ? L(l)->len : i + size.i;
    V c = list_new(e - i);
    for (int64_t j = i; j < e; j++) lpush(c, d_retain(L(l)->items[j]));
    lpush(r, c);
  }
  d_release(l);
  return r;
}
V d_n_core_List_dedup(V l) {
  V r = list_new(L(l)->len);
  for (int64_t i = 0; i < L(l)->len; i++)
    if (i == 0 || !d_eq_b(L(l)->items[i], L(l)->items[i - 1])) lpush(r, d_retain(L(l)->items[i]));
  d_release(l);
  return r;
}
V d_n_core_List_group_by(V l, V key) {
  V m = map_new(K_MAP, 4);
  for (int64_t i = 0; i < L(l)->len; i++) {
    V* slot = map_slot(M(m), call1(key, d_retain(L(l)->items[i])), 1);
    if (slot->k == K_UNIT) *slot = list_new(2);
    lpush(*slot, d_retain(L(l)->items[i]));
  }
  d_release(l); d_release(key);
  return m;
}
V d_n_core_List_to_set(V l) {
  V s = map_new(K_SET, L(l)->len);
  for (int64_t i = 0; i < L(l)->len; i++) map_put(M(s), d_retain(L(l)->items[i]), D_UNIT);
  d_release(l);
  return s;
}
V d_n_core_List_join(V l, V sep) {
  DBuf b;
  d_buf_init(&b);
  for (int64_t i = 0; i < L(l)->len; i++) {
    if (i) d_buf_add(&b, S(sep)->p, S(sep)->len);
    d_buf_add(&b, S(L(l)->items[i])->p, S(L(l)->items[i])->len);
  }
  d_release(l); d_release(sep);
  return d_buf_done(&b);
}

/* Map */
V d_n_core_Map_len(V m) { V r = d_int(M(m)->len); d_release(m); return r; }
V d_n_core_Map_is_empty(V m) { V r = d_bool(M(m)->len == 0); d_release(m); return r; }
V d_n_core_Map_get(V m, V k) {
  int64_t ix = map_find(M(m), k, d_hash(k));
  V r = ix >= 0 ? d_some(d_retain(M(m)->e[ix].val)) : d_none();
  d_release(m); d_release(k);
  return r;
}
V d_n_core_Map_contains_key(V m, V k) {
  int ok = map_find(M(m), k, d_hash(k)) >= 0;
  d_release(m); d_release(k);
  return d_bool(ok);
}
V d_n_core_Map_remove(V* m, V k) {
  map_unique(m);
  V out;
  V r = map_take(M(*m), k, &out) ? d_some(out) : d_none();
  d_release(k);
  return r;
}
static V map_list(V m, int what) {
  DMap* d = M(m);
  V r = list_new(d->len);
  for (int64_t i = 0; i < d->n; i++) {
    if (!d->e[i].live) continue;
    if (what == 0) lpush(r, d_retain(d->e[i].key));
    else if (what == 1) lpush(r, d_retain(d->e[i].val));
    else lpush(r, tup2(d_retain(d->e[i].key), d_retain(d->e[i].val)));
  }
  d_release(m);
  return r;
}
V d_n_core_Map_keys(V m) { return map_list(m, 0); }
V d_n_core_Map_values(V m) { return map_list(m, 1); }
V d_n_core_Map_entries(V m) { return map_list(m, 2); }

/* Set */
V d_n_core_Set__of(V l) { return d_n_core_List_to_set(l); }
V d_n_core_Set_len(V s) { return d_n_core_Map_len(s); }
V d_n_core_Set_contains(V s, V x) { return d_n_core_Map_contains_key(s, x); }
V d_n_core_Set_add(V* s, V x) { map_unique(s); map_put(M(*s), x, D_UNIT); return D_UNIT; }
V d_n_core_Set_remove(V* s, V x) {
  map_unique(s);
  V out;
  int ok = map_take(M(*s), x, &out);
  d_release(x);
  return d_bool(ok);
}
V d_n_core_Set_to_list(V s) { return map_list(s, 0); }
V d_n_core_Set_union(V a, V b) {
  map_unique(&a);
  for (int64_t i = 0; i < M(b)->n; i++) if (M(b)->e[i].live) map_put(M(a), d_retain(M(b)->e[i].key), D_UNIT);
  d_release(b);
  return a;
}
V d_n_core_Set_intersection(V a, V b) {
  V r = map_new(K_SET, 4);
  for (int64_t i = 0; i < M(a)->n; i++)
    if (M(a)->e[i].live && map_find(M(b), M(a)->e[i].key, M(a)->e[i].hash) >= 0) map_put(M(r), d_retain(M(a)->e[i].key), D_UNIT);
  d_release(a); d_release(b);
  return r;
}

/* Option */
V d_n_core_Option_is_some(V o) { V r = d_bool(is_some(o)); d_release(o); return r; }
V d_n_core_Option_is_none(V o) { V r = d_bool(!is_some(o)); d_release(o); return r; }
V d_n_core_Option_unwrap_or(V o, V dflt) {
  if (is_some(o)) { d_release(dflt); return d_take0(o); }
  return dflt;
}
V d_n_core_Option_map(V o, V f) {
  if (!is_some(o)) { d_release(f); return o; }
  V r = d_some(call1(f, d_take0(o)));
  d_release(f);
  return r;
}
V d_n_core_Option_and_then(V o, V f) {
  if (!is_some(o)) { d_release(f); return o; }
  V r = call1(f, d_take0(o));
  d_release(f);
  return r;
}
V d_n_core_Option_ok_or(V o, V e) {
  if (is_some(o)) { d_release(e); return d_ok(d_take0(o)); }
  return d_err(e);
}

/* Result */
V d_n_core_Result_is_ok(V r) { V x = d_bool(A(r)->tag == 0); d_release(r); return x; }
V d_n_core_Result_is_err(V r) { V x = d_bool(A(r)->tag == 1); d_release(r); return x; }
V d_n_core_Result_ok(V r) { if (A(r)->tag == 0) return d_some(d_take0(r)); d_release(r); return d_none(); }
V d_n_core_Result_unwrap_or(V r, V dflt) {
  if (A(r)->tag == 0) { d_release(dflt); return d_take0(r); }
  d_release(r);
  return dflt;
}
V d_n_core_Result_map(V r, V f) {
  if (A(r)->tag != 0) { d_release(f); return r; }
  V x = d_ok(call1(f, d_take0(r)));
  d_release(f);
  return x;
}
V d_n_core_Result_map_err(V r, V f) {
  if (A(r)->tag != 1) { d_release(f); return r; }
  V x = d_err(call1(f, d_take0(r)));
  d_release(f);
  return x;
}
V d_n_core_Result_and_then(V r, V f) {
  if (A(r)->tag != 0) { d_release(f); return r; }
  V x = call1(f, d_take0(r));
  d_release(f);
  return x;
}

/* ================================================================== natives: io, env, time, rand */

V d_n_io_println(V s) { fwrite(S(s)->p, 1, (size_t)S(s)->len, stdout); fputc('\n', stdout); d_release(s); return D_UNIT; }
V d_n_io_print(V s) { fwrite(S(s)->p, 1, (size_t)S(s)->len, stdout); d_release(s); return D_UNIT; }
V d_n_io_eprintln(V s) { fflush(stdout); fwrite(S(s)->p, 1, (size_t)S(s)->len, stderr); fputc('\n', stderr); d_release(s); return D_UNIT; }
V d_n_io_read_line(void) {
  fflush(stdout);
  char* line = NULL;
  size_t cap = 0;
  ssize_t n = getline(&line, &cap, stdin);
  if (n < 0) { free(line); return d_none(); }
  if (n && line[n - 1] == '\n') n--;
  if (n && line[n - 1] == '\r') n--;
  V r = d_some(d_str_new(line, n));
  free(line);
  return r;
}
V d_n_io_read_all(void) {
  fflush(stdout);
  DBuf b;
  d_buf_init(&b);
  char t[65536];
  size_t n;
  while ((n = fread(t, 1, sizeof t, stdin)) > 0) d_buf_add(&b, t, (int64_t)n);
  return d_buf_done(&b);
}
V d_n_env_args(void) {
  V r = list_new(d_argc);
  for (int i = 0; i < d_argc; i++) lpush(r, cstr(d_argv[i]));
  return r;
}
V d_n_env_var(V name) {
  const char* v = getenv(S(name)->p);
  d_release(name);
  return v ? d_some(cstr(v)) : d_none();
}
V d_n_time_now_ms(void) {
  struct timespec ts;
  clock_gettime(CLOCK_REALTIME, &ts);
  return d_int((int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000);
}
V d_n_time_sleep_ms(V ms) {
  fflush(stdout);
  if (ms.i > 0) { struct timespec ts = {ms.i / 1000, (ms.i % 1000) * 1000000}; nanosleep(&ts, NULL); }
  return D_UNIT;
}
static uint64_t rng_state;
static uint64_t rng(void) {
  if (!rng_state) {
    int fd = open("/dev/urandom", O_RDONLY);
    if (fd >= 0) { if (read(fd, &rng_state, 8) != 8) rng_state = 0; close(fd); }
    if (!rng_state) rng_state = (uint64_t)time(NULL) ^ ((uint64_t)getpid() << 32) ^ 0x9e3779b97f4a7c15ULL;
  }
  rng_state += 0x9e3779b97f4a7c15ULL;
  return mix(rng_state);
}
V d_n_rand_int(V lo, V hi) {
  if (lo.i >= hi.i) d_panic_c("`requires` of `int` failed: low < high");
  uint64_t span = (uint64_t)hi.i - (uint64_t)lo.i;
  uint64_t lim = UINT64_MAX - UINT64_MAX % span, x;
  do x = rng(); while (x >= lim);
  return d_int((int64_t)((uint64_t)lo.i + x % span));
}
V d_n_rand_float(void) { return d_float((double)(rng() >> 11) * 0x1.0p-53); }

/* ================================================================== natives: fs */

static V ioerr(const char* path, int err) {
  V f[2] = {cstr(strerror(err)), cstr(path)};
  return d_err(d_adt(&d_td_fs_IoError, 0, 2, f));
}
static V ioerr_msg(const char* path, const char* msg) {
  V f[2] = {cstr(msg), cstr(path)};
  return d_err(d_adt(&d_td_fs_IoError, 0, 2, f));
}
static int read_file(const char* path, DBuf* b) {
  int fd = open(path, O_RDONLY);
  if (fd < 0) return errno;
  d_buf_init(b);
  char t[65536];
  ssize_t n;
  while ((n = read(fd, t, sizeof t)) > 0) d_buf_add(b, t, n);
  int e = n < 0 ? errno : 0;
  close(fd);
  if (e) free(b->p);
  return e;
}
V d_n_fs_read_str(V path) {
  DBuf b;
  int e = read_file(S(path)->p, &b);
  V r;
  if (e) r = ioerr(S(path)->p, e);
  else if (!utf8_valid(b.p, b.len)) { free(b.p); r = ioerr_msg(S(path)->p, "file is not valid UTF-8 text; use fs.read_bytes"); }
  else r = d_ok(d_buf_done(&b));
  d_release(path);
  return r;
}
V d_n_fs_read_bytes(V path) {
  DBuf b;
  int e = read_file(S(path)->p, &b);
  V r = e ? ioerr(S(path)->p, e) : d_ok(buf_done_kind(&b, K_BYTES));
  d_release(path);
  return r;
}
static int write_all(int fd, const char* p, int64_t n) {
  while (n > 0) {
    ssize_t w = write(fd, p, (size_t)n);
    if (w < 0) { if (errno == EINTR) continue; return errno; }
    p += w; n -= w;
  }
  return 0;
}
static V write_file(V path, V data) {
  int fd = open(S(path)->p, O_WRONLY | O_CREAT | O_TRUNC, 0644);
  V r;
  if (fd < 0) r = ioerr(S(path)->p, errno);
  else {
    int e = write_all(fd, S(data)->p, S(data)->len);
    close(fd);
    r = e ? ioerr(S(path)->p, e) : d_ok(D_UNIT);
  }
  d_release(path); d_release(data);
  return r;
}
V d_n_fs_write_str(V path, V text) { return write_file(path, text); }
V d_n_fs_write_bytes(V path, V data) { return write_file(path, data); }
V d_n_fs_exists(V path) { struct stat st; int ok = stat(S(path)->p, &st) == 0; d_release(path); return d_bool(ok); }
static int cmp_names(const void* a, const void* b) { return d_cmp_b(*(const V*)a, *(const V*)b); }
V d_n_fs_list_dir(V path) {
  DIR* d = opendir(S(path)->p);
  if (!d) { V r = ioerr(S(path)->p, errno); d_release(path); return r; }
  V l = list_new(8);
  struct dirent* e;
  while ((e = readdir(d))) if (strcmp(e->d_name, ".") && strcmp(e->d_name, "..")) lpush(l, cstr(e->d_name));
  closedir(d);
  qsort(L(l)->items, (size_t)L(l)->len, sizeof(V), cmp_names);
  d_release(path);
  return d_ok(l);
}
V d_n_fs_remove(V path) {
  V r = d_ok(D_UNIT);
  if (unlink(S(path)->p) != 0) {
    if ((errno == EISDIR || errno == EPERM) && rmdir(S(path)->p) == 0) r = d_ok(D_UNIT);
    else r = ioerr(S(path)->p, errno);
  }
  d_release(path);
  return r;
}
V d_n_fs_temp_path(V prefix) {
  const char* dir = getenv("TMPDIR");
  if (!dir || !*dir) dir = "/tmp";
  char t[4096];
  snprintf(t, sizeof t, "%s/%s-XXXXXX", dir, S(prefix)->p);
  d_release(prefix);
  int fd = mkstemp(t);
  if (fd < 0) return ioerr(t, errno);
  close(fd);
  return d_ok(cstr(t));
}
V d_n_fs_open(V path, V mode) {
  static const int flags[] = {O_RDONLY, O_WRONLY | O_CREAT | O_TRUNC, O_WRONLY | O_CREAT | O_APPEND, O_RDWR | O_CREAT | O_APPEND};
  int fd = open(S(path)->p, flags[A(mode)->tag] | O_CLOEXEC, 0644);
  V r;
  if (fd < 0) r = ioerr(S(path)->p, errno);
  else {
    DHandle* h = newobj(sizeof(DHandle));
    h->fd = fd;
    h->path = strdup(S(path)->p);
    r = d_ok(mk(K_HANDLE, h));
  }
  d_release(path); d_release(mode);
  return r;
}
#define H(v) ((DHandle*)(v).o)
V d_n_fs_File_read_at(V f, V off, V n) {
  if (off.i < 0 || n.i < 0) d_panic_c("negative offset or length in `read_at`");
  V b = str_alloc(K_BYTES, n.i);
  int64_t got = 0;
  while (got < n.i) {
    ssize_t r = pread(H(f)->fd, S(b)->p + got, (size_t)(n.i - got), off.i + got);
    if (r < 0) { if (errno == EINTR) continue; V e = ioerr(H(f)->path, errno); d_release(b); d_release(f); return e; }
    if (r == 0) break;
    got += r;
  }
  S(b)->len = got;
  S(b)->p[got] = 0;
  d_release(f);
  return d_ok(b);
}
V d_n_fs_File_append(V f, V data) {
  int e = write_all(H(f)->fd, S(data)->p, S(data)->len);
  V r = e ? ioerr(H(f)->path, e) : d_ok(D_UNIT);
  d_release(f); d_release(data);
  return r;
}
V d_n_fs_File_len(V f) {
  struct stat st;
  V r = fstat(H(f)->fd, &st) == 0 ? d_ok(d_int((int64_t)st.st_size)) : ioerr(H(f)->path, errno);
  d_release(f);
  return r;
}
V d_n_fs_File_sync(V f) {
  V r = fsync(H(f)->fd) == 0 ? d_ok(D_UNIT) : ioerr(H(f)->path, errno);
  d_release(f);
  return r;
}
