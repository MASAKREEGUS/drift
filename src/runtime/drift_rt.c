/* Drift runtime. Ownership rule: every function receives owned values and must release or keep them;
   `V*` parameters (var self) are borrowed slots that may be mutated after copy-on-write. */
#define _GNU_SOURCE
#include "drift_rt.h"
#include <stdio.h>
#include <stdlib.h>
#include <stdarg.h>
#include <ctype.h>
#include <errno.h>
#include <unistd.h>
#include <fcntl.h>
#include <dirent.h>
#include <time.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <sys/socket.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <dlfcn.h>
#include <signal.h>
#include <sys/random.h>
#include <sys/prctl.h>
#include <sys/syscall.h>

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
/* borrows key; the value slot or NULL */
static V* map_lookup(DMap* m, V key) {
  int64_t ix = map_find(m, key, d_hash(key));
  return ix >= 0 ? &m->e[ix].val : NULL;
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
static void html_show(DBuf* b, V v);
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
    if (a->t == &d_td_re_Regex) { buf_cstr(b, "Regex("); show_quoted(b, S(a->f[0])->p, S(a->f[0])->len, 0); buf_ch(b, ')'); break; }
    if (a->t == &d_td_html_Document || a->t == &d_td_html_Node) { html_show(b, v); break; }
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
static const char* const f_response[] = {"status", "headers", "body"};
static const char* const f_regex[] = {"pattern", "program"};
static const char* const f_match[] = {"text", "start", "end", "groups"};
static const char* const f_doc[] = {"data"};
static const char* const f_node[] = {"doc", "index"};
static const DVariant v_option[] = {{"Some", 1, NULL}, {"None", 0, NULL}};
static const DVariant v_result[] = {{"Ok", 1, NULL}, {"Err", 1, NULL}};
static const DVariant v_error[] = {{NULL, 1, f_message}};
static const DVariant v_ioerror[] = {{NULL, 2, f_ioerror}};
static const DVariant v_mode[] = {{"Read", 0, NULL}, {"Write", 0, NULL}, {"Append", 0, NULL}, {"ReadAppendCreate", 0, NULL}};
static const DVariant v_httperror[] = {{NULL, 2, f_httperror}};
static const DVariant v_request[] = {{NULL, 5, f_request}};
static const DVariant v_response[] = {{NULL, 3, f_response}};
static const DVariant v_regex[] = {{NULL, 2, f_regex}};
static const DVariant v_match[] = {{NULL, 4, f_match}};
static const DVariant v_doc[] = {{NULL, 1, f_doc}};
static const DVariant v_node[] = {{NULL, 2, f_node}};
static const DVariant v_jsonerror[] = {{NULL, 1, f_message}};
static const DVariant v_message1[] = {{NULL, 1, f_message}};
DType d_td_core_Option = {"Option", 2, v_option, 1, 1, NULL};
DType d_td_core_Result = {"Result", 2, v_result, 1, 1, NULL};
DType d_td_core_Error = {"Error", 1, v_error, 0, 0, NULL};
DType d_td_fs_IoError = {"IoError", 1, v_ioerror, 0, 0, NULL};
DType d_td_fs_Mode = {"Mode", 4, v_mode, 1, 0, NULL};
DType d_td_http_HttpError = {"HttpError", 1, v_httperror, 0, 0, NULL};
DType d_td_http_Request = {"Request", 1, v_request, 0, 0, NULL};
DType d_td_http_Response = {"Response", 1, v_response, 0, 0, NULL};
DType d_td_re_Regex = {"Regex", 1, v_regex, 0, 0, NULL};
DType d_td_re_RegexError = {"RegexError", 1, v_message1, 0, 0, NULL};
DType d_td_re_Match = {"Match", 1, v_match, 0, 0, NULL};
DType d_td_html_Document = {"Document", 1, v_doc, 0, 0, NULL};
DType d_td_html_Node = {"Node", 1, v_node, 0, 0, NULL};
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
      d_sandbox(t[i].effects);
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
    if (getrandom(&rng_state, sizeof rng_state, 0) != (ssize_t)sizeof rng_state) rng_state = 0;
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

/* ------------------------------------------------------------------ json
   Mapping: Int/Float -> number, Bool, Str -> string, Bytes -> base64 string, () and None -> null,
   Some(x) -> x, List/Set/tuple -> array, Map[Str, V] -> object, other maps -> [[k, v], ...],
   struct -> object, unit variant -> "Name", other variants -> {"Name": fields}, Ok/Err -> {"ok": x}/{"err": e}. */

static const char b64c[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
static void b64_enc(DBuf* b, const unsigned char* p, int64_t n) {
  for (int64_t i = 0; i < n; i += 3) {
    uint32_t x = (uint32_t)p[i] << 16 | (i + 1 < n ? (uint32_t)p[i + 1] << 8 : 0) | (i + 2 < n ? p[i + 2] : 0);
    char q[4] = {b64c[x >> 18 & 63], b64c[x >> 12 & 63], i + 1 < n ? b64c[x >> 6 & 63] : '=', i + 2 < n ? b64c[x & 63] : '='};
    d_buf_add(b, q, 4);
  }
}
static int b64_val(char c) {
  if (c >= 'A' && c <= 'Z') return c - 'A';
  if (c >= 'a' && c <= 'z') return c - 'a' + 26;
  if (c >= '0' && c <= '9') return c - '0' + 52;
  if (c == '+' || c == '-') return 62;
  if (c == '/' || c == '_') return 63;
  return -1;
}
static int b64_dec(const char* s, int64_t n, DBuf* out) {
  while (n > 0 && s[n - 1] == '=') n--;
  if (n % 4 == 1) return 0;
  uint32_t acc = 0; int bits = 0;
  for (int64_t i = 0; i < n; i++) {
    int v = b64_val(s[i]);
    if (v < 0) return 0;
    acc = acc << 6 | (uint32_t)v; bits += 6;
    if (bits >= 8) { bits -= 8; char c = (char)(acc >> bits & 0xff); d_buf_add(out, &c, 1); }
  }
  return 1;
}

static void json_str(DBuf* b, const char* p, int64_t n) {
  buf_ch(b, '"');
  for (int64_t i = 0; i < n; i++) {
    unsigned char c = (unsigned char)p[i];
    switch (c) {
    case '"': buf_cstr(b, "\\\""); break;
    case '\\': buf_cstr(b, "\\\\"); break;
    case '\n': buf_cstr(b, "\\n"); break;
    case '\r': buf_cstr(b, "\\r"); break;
    case '\t': buf_cstr(b, "\\t"); break;
    case '\b': buf_cstr(b, "\\b"); break;
    case '\f': buf_cstr(b, "\\f"); break;
    default:
      if (c < 0x20 || c == 0x7f) buf_fmt(b, "\\u%04x", c); else buf_ch(b, (char)c);
    }
  }
  buf_ch(b, '"');
}
static void json_enc(DBuf* b, V v) {
  switch (v.k) {
  case K_UNIT: buf_cstr(b, "null"); break;
  case K_BOOL: buf_cstr(b, v.i ? "true" : "false"); break;
  case K_INT: buf_fmt(b, "%lld", (long long)v.i); break;
  case K_FLOAT: if (isfinite(v.f)) show_float(b, v.f); else buf_cstr(b, "null"); break;
  case K_STR: json_str(b, S(v)->p, S(v)->len); break;
  case K_BYTES: buf_ch(b, '"'); b64_enc(b, (const unsigned char*)S(v)->p, S(v)->len); buf_ch(b, '"'); break;
  case K_LIST:
    buf_ch(b, '[');
    for (int64_t i = 0; i < L(v)->len; i++) { if (i) buf_ch(b, ','); json_enc(b, L(v)->items[i]); }
    buf_ch(b, ']');
    break;
  case K_TUPLE:
    buf_ch(b, '[');
    for (int64_t i = 0; i < T(v)->n; i++) { if (i) buf_ch(b, ','); json_enc(b, T(v)->items[i]); }
    buf_ch(b, ']');
    break;
  case K_SET: case K_MAP: {
    DMap* m = M(v);
    int obj = v.k == K_MAP;
    for (int64_t i = 0; i < m->n && obj; i++) if (m->e[i].live && m->e[i].key.k != K_STR) obj = 0;
    buf_ch(b, obj ? '{' : '[');
    int first = 1;
    for (int64_t i = 0; i < m->n; i++) {
      if (!m->e[i].live) continue;
      if (!first) buf_ch(b, ',');
      first = 0;
      if (v.k == K_SET) json_enc(b, m->e[i].key);
      else if (obj) { json_enc(b, m->e[i].key); buf_ch(b, ':'); json_enc(b, m->e[i].val); }
      else { buf_ch(b, '['); json_enc(b, m->e[i].key); buf_ch(b, ','); json_enc(b, m->e[i].val); buf_ch(b, ']'); }
    }
    buf_ch(b, obj ? '}' : ']');
    break;
  }
  case K_ADT: {
    DAdt* a = A(v);
    const DVariant* var = &a->t->v[a->tag];
    if (a->t == &d_td_core_Option) { if (a->tag == 0) json_enc(b, a->f[0]); else buf_cstr(b, "null"); break; }
    if (a->t == &d_td_core_Result) { buf_cstr(b, a->tag == 0 ? "{\"ok\":" : "{\"err\":"); json_enc(b, a->f[0]); buf_ch(b, '}'); break; }
    if (a->t->is_enum) {
      if (a->n == 0 && !var->fields) { json_str(b, var->name, (int64_t)strlen(var->name)); break; }
      buf_ch(b, '{'); json_str(b, var->name, (int64_t)strlen(var->name)); buf_ch(b, ':');
    }
    if (var->fields) {
      buf_ch(b, '{');
      for (int i = 0; i < a->n; i++) {
        if (i) buf_ch(b, ',');
        json_str(b, var->fields[i], (int64_t)strlen(var->fields[i]));
        buf_ch(b, ':');
        json_enc(b, a->f[i]);
      }
      buf_ch(b, '}');
    } else if (a->n == 1) json_enc(b, a->f[0]);
    else {
      buf_ch(b, '[');
      for (int i = 0; i < a->n; i++) { if (i) buf_ch(b, ','); json_enc(b, a->f[i]); }
      buf_ch(b, ']');
    }
    if (a->t->is_enum) buf_ch(b, '}');
    break;
  }
  default: d_panic_c("json.encode: functions, handles, ranges and tasks are not data");
  }
}
V d_n_json_encode(V v) {
  DBuf b; d_buf_init(&b);
  json_enc(&b, v);
  d_release(v);
  return d_buf_done(&b);
}

/* parse into a tree first, then convert guided by the target type */
enum { J_NULL, J_FALSE, J_TRUE, J_NUM, J_STR, J_ARR, J_OBJ };
typedef struct JNode {
  int t;
  int64_t n;             /* string length, element or member count */
  char* s;               /* J_STR: decoded UTF-8; J_NUM: points into the source */
  struct JNode* kids;    /* J_ARR elements, J_OBJ values */
  char** keys; int64_t* klens;
  int num_int;           /* J_NUM written without fraction or exponent */
} JNode;
typedef struct JP { const char* p; const char* end; const char* start; char err[256]; int depth; } JP;

static void jfree(JNode* n) {
  if (n->t == J_STR) free(n->s);
  if (n->t == J_ARR || n->t == J_OBJ) {
    for (int64_t i = 0; i < n->n; i++) { jfree(&n->kids[i]); if (n->keys) free(n->keys[i]); }
    free(n->kids); free(n->keys); free(n->klens);
  }
}
static int jfail(JP* j, const char* what) {
  if (j->err[0]) return 0;
  int line = 1, col = 1;
  for (const char* q = j->start; q < j->p && q < j->end; q++) { if (*q == '\n') { line++; col = 1; } else col++; }
  snprintf(j->err, sizeof j->err, "invalid JSON at %d:%d: %s", line, col, what);
  return 0;
}
static void jws(JP* j) { while (j->p < j->end && (*j->p == ' ' || *j->p == '\t' || *j->p == '\n' || *j->p == '\r')) j->p++; }
static int jhex4(JP* j, uint32_t* out) {
  if (j->end - j->p < 4) return jfail(j, "short \\u escape");
  uint32_t x = 0;
  for (int i = 0; i < 4; i++) {
    char c = j->p[i]; x <<= 4;
    if (c >= '0' && c <= '9') x |= (uint32_t)(c - '0');
    else if (c >= 'a' && c <= 'f') x |= (uint32_t)(c - 'a' + 10);
    else if (c >= 'A' && c <= 'F') x |= (uint32_t)(c - 'A' + 10);
    else return jfail(j, "bad hex digit in \\u escape");
  }
  j->p += 4; *out = x; return 1;
}
static void utf8_put(DBuf* b, uint32_t c) {
  char q[4]; int n;
  if (c < 0x80) { q[0] = (char)c; n = 1; }
  else if (c < 0x800) { q[0] = (char)(0xc0 | c >> 6); q[1] = (char)(0x80 | (c & 63)); n = 2; }
  else if (c < 0x10000) { q[0] = (char)(0xe0 | c >> 12); q[1] = (char)(0x80 | (c >> 6 & 63)); q[2] = (char)(0x80 | (c & 63)); n = 3; }
  else { q[0] = (char)(0xf0 | c >> 18); q[1] = (char)(0x80 | (c >> 12 & 63)); q[2] = (char)(0x80 | (c >> 6 & 63)); q[3] = (char)(0x80 | (c & 63)); n = 4; }
  d_buf_add(b, q, n);
}
static int jstring(JP* j, char** out, int64_t* len) {
  j->p++; /* opening quote */
  DBuf b; d_buf_init(&b);
  for (;;) {
    if (j->p >= j->end) { free(b.p); return jfail(j, "unterminated string"); }
    unsigned char c = (unsigned char)*j->p;
    if (c == '"') { j->p++; break; }
    if (c < 0x20) { free(b.p); return jfail(j, "control character in string (escape it)"); }
    if (c != '\\') { const char* s = j->p; while (j->p < j->end && *j->p != '"' && *j->p != '\\' && (unsigned char)*j->p >= 0x20) j->p++; d_buf_add(&b, s, j->p - s); continue; }
    j->p++;
    if (j->p >= j->end) { free(b.p); return jfail(j, "unterminated escape"); }
    char e = *j->p++;
    switch (e) {
    case '"': buf_ch(&b, '"'); break;
    case '\\': buf_ch(&b, '\\'); break;
    case '/': buf_ch(&b, '/'); break;
    case 'b': buf_ch(&b, '\b'); break;
    case 'f': buf_ch(&b, '\f'); break;
    case 'n': buf_ch(&b, '\n'); break;
    case 'r': buf_ch(&b, '\r'); break;
    case 't': buf_ch(&b, '\t'); break;
    case 'u': {
      uint32_t u;
      if (!jhex4(j, &u)) { free(b.p); return 0; }
      if (u >= 0xd800 && u < 0xdc00) {
        uint32_t lo;
        if (j->end - j->p < 2 || j->p[0] != '\\' || j->p[1] != 'u') { free(b.p); return jfail(j, "lone surrogate in \\u escape"); }
        j->p += 2;
        if (!jhex4(j, &lo)) { free(b.p); return 0; }
        if (lo < 0xdc00 || lo >= 0xe000) { free(b.p); return jfail(j, "bad surrogate pair"); }
        u = 0x10000 + ((u - 0xd800) << 10) + (lo - 0xdc00);
      } else if (u >= 0xdc00 && u < 0xe000) { free(b.p); return jfail(j, "lone surrogate in \\u escape"); }
      utf8_put(&b, u);
      break;
    }
    default: free(b.p); return jfail(j, "unknown escape");
    }
  }
  if (!utf8_valid(b.p, b.len)) { free(b.p); return jfail(j, "string is not valid UTF-8"); }
  buf_ch(&b, 0);
  *out = b.p; *len = b.len - 1;
  return 1;
}
static int jvalue(JP* j, JNode* n) {
  memset(n, 0, sizeof *n);
  jws(j);
  if (j->p >= j->end) return jfail(j, "unexpected end of input");
  char c = *j->p;
  if (c == '{' || c == '[') {
    if (++j->depth > 512) return jfail(j, "nesting deeper than 512 levels");
    int obj = c == '{';
    n->t = obj ? J_OBJ : J_ARR;
    j->p++;
    int64_t cap = 0;
    jws(j);
    if (j->p < j->end && *j->p == (obj ? '}' : ']')) { j->p++; j->depth--; return 1; }
    for (;;) {
      if (n->n == cap) {
        cap = cap ? cap * 2 : 4;
        n->kids = xrealloc(n->kids, (size_t)cap * sizeof(JNode));
        if (obj) { n->keys = xrealloc(n->keys, (size_t)cap * sizeof(char*)); n->klens = xrealloc(n->klens, (size_t)cap * sizeof(int64_t)); }
      }
      if (obj) {
        jws(j);
        if (j->p >= j->end || *j->p != '"') return jfail(j, "expected a string key");
        if (!jstring(j, &n->keys[n->n], &n->klens[n->n])) return 0;
        jws(j);
        if (j->p >= j->end || *j->p != ':') { free(n->keys[n->n]); return jfail(j, "expected `:` after key"); }
        j->p++;
      }
      if (!jvalue(j, &n->kids[n->n])) { if (obj) free(n->keys[n->n]); jfree(&n->kids[n->n]); return 0; }
      n->n++;
      jws(j);
      if (j->p < j->end && *j->p == ',') { j->p++; continue; }
      if (j->p < j->end && *j->p == (obj ? '}' : ']')) { j->p++; break; }
      return jfail(j, obj ? "expected `,` or `}`" : "expected `,` or `]`");
    }
    j->depth--;
    return 1;
  }
  if (c == '"') { n->t = J_STR; return jstring(j, &n->s, &n->n); }
  if (c == '-' || (c >= '0' && c <= '9')) {
    const char* s = j->p;
    int is_int = 1;
    if (*j->p == '-') j->p++;
    if (j->p < j->end && *j->p == '0') j->p++;
    else if (j->p < j->end && *j->p >= '1' && *j->p <= '9') while (j->p < j->end && *j->p >= '0' && *j->p <= '9') j->p++;
    else return jfail(j, "bad number");
    if (j->p < j->end && *j->p == '.') {
      is_int = 0; j->p++;
      if (j->p >= j->end || *j->p < '0' || *j->p > '9') return jfail(j, "bad number: digits expected after `.`");
      while (j->p < j->end && *j->p >= '0' && *j->p <= '9') j->p++;
    }
    if (j->p < j->end && (*j->p == 'e' || *j->p == 'E')) {
      is_int = 0; j->p++;
      if (j->p < j->end && (*j->p == '+' || *j->p == '-')) j->p++;
      if (j->p >= j->end || *j->p < '0' || *j->p > '9') return jfail(j, "bad number: digits expected in exponent");
      while (j->p < j->end && *j->p >= '0' && *j->p <= '9') j->p++;
    }
    n->t = J_NUM; n->s = (char*)s; n->n = j->p - s; n->num_int = is_int;
    return 1;
  }
  static const struct { const char* w; int t; } lits[] = {{"null", J_NULL}, {"true", J_TRUE}, {"false", J_FALSE}};
  for (int i = 0; i < 3; i++) {
    size_t l = strlen(lits[i].w);
    if ((size_t)(j->end - j->p) >= l && !memcmp(j->p, lits[i].w, l)) { j->p += l; n->t = lits[i].t; return 1; }
  }
  return jfail(j, "unexpected character");
}

typedef struct JC { DBuf path; char err[512]; } JC;
static const char* jkind(const JNode* n) {
  static const char* const k[] = {"null", "false", "true", "number", "string", "array", "object"};
  return k[n->t];
}
static void ty_show(DBuf* b, const DTy* t) {
  static const char* const ints[] = {"Int", "I32", "U8", "U32", "U64"};
  switch (t->k) {
  case K_UNIT: buf_cstr(b, "()"); break;
  case K_BOOL: buf_cstr(b, "Bool"); break;
  case K_INT: buf_cstr(b, ints[t->num]); break;
  case K_FLOAT: buf_cstr(b, t->num ? "F32" : "Float"); break;
  case K_STR: buf_cstr(b, "Str"); break;
  case K_BYTES: buf_cstr(b, "Bytes (base64 string)"); break;
  case K_LIST: buf_cstr(b, "List["); ty_show(b, t->a[0]); buf_ch(b, ']'); break;
  case K_SET: buf_cstr(b, "Set["); ty_show(b, t->a[0]); buf_ch(b, ']'); break;
  case K_MAP: buf_cstr(b, "Map["); ty_show(b, t->a[0]); buf_cstr(b, ", "); ty_show(b, t->a[1]); buf_ch(b, ']'); break;
  case K_TUPLE:
    buf_ch(b, '(');
    for (int64_t i = 0; i < t->n; i++) { if (i) buf_cstr(b, ", "); ty_show(b, t->a[i]); }
    buf_ch(b, ')');
    break;
  default: buf_cstr(b, t->t->name);
  }
}
static int jc_fail(JC* c, const char* fmt, ...) {
  if (c->err[0]) return 0;
  char m[300];
  va_list ap; va_start(ap, fmt); vsnprintf(m, sizeof m, fmt, ap); va_end(ap);
  snprintf(c->err, sizeof c->err, "at %.*s: %s", (int)c->path.len, c->path.p, m);
  return 0;
}
static int jc_expected(JC* c, const DTy* t, const JNode* n) {
  DBuf b; d_buf_init(&b); ty_show(&b, t); buf_ch(&b, 0);
  jc_fail(c, "expected %s, found %s", b.p, jkind(n));
  free(b.p);
  return 0;
}
static int jconv(JC* c, const JNode* n, const DTy* t, V* out);
static int jconv_at_index(JC* c, const JNode* n, const DTy* t, V* out, int64_t i) {
  int64_t mark = c->path.len;
  buf_fmt(&c->path, "[%lld]", (long long)i);
  int ok = jconv(c, n, t, out);
  c->path.len = mark;
  return ok;
}
static int jconv_at_key(JC* c, const JNode* n, const DTy* t, V* out, const char* k, int64_t kl) {
  int64_t mark = c->path.len;
  int plain = kl > 0;
  for (int64_t i = 0; i < kl; i++) {
    char ch = k[i];
    if (!((ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z') || ch == '_' || (i && ch >= '0' && ch <= '9'))) plain = 0;
  }
  if (plain) { buf_ch(&c->path, '.'); d_buf_add(&c->path, k, kl); }
  else { buf_ch(&c->path, '['); json_str(&c->path, k, kl); buf_ch(&c->path, ']'); }
  int ok = jconv(c, n, t, out);
  c->path.len = mark;
  return ok;
}
static const JNode* jget(const JNode* obj, const char* k) {
  int64_t kl = (int64_t)strlen(k);
  const JNode* found = NULL;
  for (int64_t i = 0; i < obj->n; i++) if (obj->klens[i] == kl && !memcmp(obj->keys[i], k, (size_t)kl)) found = &obj->kids[i];
  return found; /* last duplicate wins */
}
static void release_all(V* vs, int64_t n) { for (int64_t i = 0; i < n; i++) d_release(vs[i]); }
/* fields of variant `vi` from an object (named), a value (one positional field) or an array */
static int jfields(JC* c, const JNode* n, const DTy* t, int vi, V* out) {
  const DVariant* var = &t->t->v[vi];
  const DTy* const* ft = t->vf[vi];
  int nf = var->n;
  V* f = nf ? xmalloc((size_t)nf * sizeof(V)) : NULL;
  int ok = 1;
  if (var->fields) {
    if (n->t != J_OBJ) { free(f); DBuf b; d_buf_init(&b); buf_cstr(&b, "an object for "); ty_show(&b, t); if (t->t->is_enum) { buf_ch(&b, '.'); buf_cstr(&b, var->name); } buf_ch(&b, 0); jc_fail(c, "expected %s, found %s", b.p, jkind(n)); free(b.p); return 0; }
    for (int i = 0; i < nf && ok; i++) {
      const JNode* m = jget(n, var->fields[i]);
      if (!m) {
        if (ft[i]->k == K_ADT && ft[i]->t == &d_td_core_Option) { f[i] = d_none(); continue; }
        ok = jc_fail(c, "missing field `%s`", var->fields[i]);
        release_all(f, i);
        break;
      }
      if (!jconv_at_key(c, m, ft[i], &f[i], var->fields[i], (int64_t)strlen(var->fields[i]))) { release_all(f, i); ok = 0; }
    }
  } else if (nf == 1) {
    ok = jconv(c, n, ft[0], &f[0]);
  } else if (nf > 1) {
    if (n->t != J_ARR || n->n != nf) { free(f); return jc_fail(c, "expected an array of %d values for `%s`, found %s", nf, var->name, jkind(n)); }
    for (int i = 0; i < nf && ok; i++) if (!jconv_at_index(c, &n->kids[i], ft[i], &f[i], i)) { release_all(f, i); ok = 0; }
  }
  if (ok) *out = d_adt(t->t, vi, nf, f);
  free(f);
  return ok;
}
static int jconv(JC* c, const JNode* n, const DTy* t, V* out) {
  switch (t->k) {
  case K_UNIT: if (n->t != J_NULL) return jc_expected(c, t, n); *out = D_UNIT; return 1;
  case K_BOOL: if (n->t != J_TRUE && n->t != J_FALSE) return jc_expected(c, t, n); *out = d_bool(n->t == J_TRUE); return 1;
  case K_INT: {
    if (n->t != J_NUM) return jc_expected(c, t, n);
    char tmp[64];
    if (!n->num_int || n->n >= (int64_t)sizeof tmp) {
      if (!n->num_int) return jc_fail(c, "expected an integer, found %.*s", (int)(n->n > 40 ? 40 : n->n), n->s);
      return jc_fail(c, "integer out of range");
    }
    memcpy(tmp, n->s, (size_t)n->n); tmp[n->n] = 0;
    errno = 0;
    static const int64_t lo[] = {INT64_MIN, INT32_MIN, 0, 0, 0};
    static const int64_t hi[] = {INT64_MAX, INT32_MAX, 255, 4294967295LL, INT64_MAX};
    if (t->num == 4 && tmp[0] != '-') {
      unsigned long long u = strtoull(tmp, NULL, 10);
      if (errno) return jc_fail(c, "%s does not fit in U64", tmp);
      *out = d_int((int64_t)u); return 1;
    }
    long long x = strtoll(tmp, NULL, 10);
    if (errno || x < lo[t->num] || x > hi[t->num]) { DBuf b; d_buf_init(&b); ty_show(&b, t); buf_ch(&b, 0); jc_fail(c, "%s does not fit in %s", tmp, b.p); free(b.p); return 0; }
    *out = d_int(x); return 1;
  }
  case K_FLOAT: {
    if (n->t != J_NUM) return jc_expected(c, t, n);
    char tmp[512];
    int64_t l = n->n < (int64_t)sizeof tmp - 1 ? n->n : (int64_t)sizeof tmp - 1;
    memcpy(tmp, n->s, (size_t)l); tmp[l] = 0;
    double f = strtod(tmp, NULL);
    *out = d_float(t->num ? (double)(float)f : f); return 1;
  }
  case K_STR: if (n->t != J_STR) return jc_expected(c, t, n); *out = str_from(K_STR, n->s, n->n); return 1;
  case K_BYTES: {
    if (n->t != J_STR) return jc_expected(c, t, n);
    DBuf b; d_buf_init(&b);
    if (!b64_dec(n->s, n->n, &b)) { free(b.p); return jc_fail(c, "invalid base64 for Bytes"); }
    *out = buf_done_kind(&b, K_BYTES); return 1;
  }
  case K_LIST: case K_SET: {
    if (n->t != J_ARR) return jc_expected(c, t, n);
    V l = list_new(n->n);
    for (int64_t i = 0; i < n->n; i++) {
      V x;
      if (!jconv_at_index(c, &n->kids[i], t->a[0], &x, i)) { d_release(l); return 0; }
      lpush(l, x);
    }
    *out = t->k == K_SET ? d_n_core_List_to_set(l) : l;
    return 1;
  }
  case K_TUPLE: {
    if (n->t != J_ARR || n->n != t->n) {
      if (n->t == J_ARR) return jc_fail(c, "expected an array of %lld values, found %lld", (long long)t->n, (long long)n->n);
      return jc_expected(c, t, n);
    }
    V* items = xmalloc((size_t)t->n * sizeof(V));
    for (int64_t i = 0; i < t->n; i++) if (!jconv_at_index(c, &n->kids[i], t->a[i], &items[i], i)) { release_all(items, i); free(items); return 0; }
    *out = d_tuple(t->n, items);
    free(items);
    return 1;
  }
  case K_MAP: {
    V m = map_new(K_MAP, n->n);
    if (n->t == J_OBJ && (t->a[0]->k == K_STR || n->n == 0)) {
      for (int64_t i = 0; i < n->n; i++) {
        V x;
        if (!jconv_at_key(c, &n->kids[i], t->a[1], &x, n->keys[i], n->klens[i])) { d_release(m); return 0; }
        map_put(M(m), str_from(K_STR, n->keys[i], n->klens[i]), x);
      }
    } else if (n->t == J_ARR && t->a[0]->k != K_STR) {
      for (int64_t i = 0; i < n->n; i++) {
        const JNode* p = &n->kids[i];
        int64_t mark = c->path.len;
        buf_fmt(&c->path, "[%lld]", (long long)i);
        if (p->t != J_ARR || p->n != 2) { jc_fail(c, "expected a [key, value] pair, found %s", jkind(p)); d_release(m); return 0; }
        V k, x;
        if (!jconv_at_index(c, &p->kids[0], t->a[0], &k, 0)) { d_release(m); return 0; }
        if (!jconv_at_index(c, &p->kids[1], t->a[1], &x, 1)) { d_release(k); d_release(m); return 0; }
        c->path.len = mark;
        map_put(M(m), k, x);
      }
    } else {
      d_release(m);
      return jc_fail(c, "expected %s, found %s", t->a[0]->k == K_STR ? "an object" : "an array of [key, value] pairs", jkind(n));
    }
    *out = m;
    return 1;
  }
  case K_ADT: {
    DType* dt = t->t;
    if (dt == &d_td_core_Option) {
      if (n->t == J_NULL) { *out = d_none(); return 1; }
      V x;
      if (!jconv(c, n, t->vf[0][0], &x)) return 0;
      *out = d_some(x); return 1;
    }
    if (dt == &d_td_core_Result) {
      const JNode* ok = n->t == J_OBJ && n->n == 1 ? jget(n, "ok") : NULL;
      const JNode* er = n->t == J_OBJ && n->n == 1 ? jget(n, "err") : NULL;
      if (!ok && !er) return jc_fail(c, "expected {\"ok\": ...} or {\"err\": ...}, found %s", jkind(n));
      V x;
      if (!jconv_at_key(c, ok ? ok : er, t->vf[ok ? 0 : 1][0], &x, ok ? "ok" : "err", ok ? 2 : 3)) return 0;
      *out = ok ? d_ok(x) : d_err(x); return 1;
    }
    if (!dt->is_enum) return jfields(c, n, t, 0, out);
    const char* name; int64_t nl; const JNode* body = NULL;
    if (n->t == J_STR) { name = n->s; nl = n->n; }
    else if (n->t == J_OBJ && n->n == 1) { name = n->keys[0]; nl = n->klens[0]; body = &n->kids[0]; }
    else return jc_fail(c, "expected a `%s` variant (\"Name\" or {\"Name\": ...}), found %s", dt->name, jkind(n));
    for (int vi = 0; vi < dt->nv; vi++) {
      const DVariant* var = &dt->v[vi];
      if ((int64_t)strlen(var->name) != nl || memcmp(var->name, name, (size_t)nl)) continue;
      if (var->n == 0) {
        if (body && body->t != J_NULL && !(body->t == J_OBJ && body->n == 0)) return jc_fail(c, "variant `%s` has no fields", var->name);
        *out = d_adt(dt, vi, 0, NULL); return 1;
      }
      if (!body) return jc_fail(c, "variant `%s` needs fields: {\"%s\": ...}", var->name, var->name);
      int64_t mark = c->path.len;
      buf_ch(&c->path, '.'); buf_cstr(&c->path, var->name);
      int r = jfields(c, body, t, vi, out);
      c->path.len = mark;
      return r;
    }
    return jc_fail(c, "`%.*s` is not a variant of `%s`", (int)(nl > 60 ? 60 : nl), name, dt->name);
  }
  default: return jc_fail(c, "this type cannot be decoded");
  }
}
static V json_err(const char* msg) {
  V m = str_from(K_STR, msg, (int64_t)strlen(msg));
  return d_err(d_adt(&d_td_json_JsonError, 0, 1, &m));
}
V d_n_json_decode(V text, const DTy* ty) {
  JP j = {S(text)->p, S(text)->p + S(text)->len, S(text)->p, {0}, 0};
  JNode root;
  V r;
  if (!jvalue(&j, &root)) { jfree(&root); r = json_err(j.err); }
  else {
    jws(&j);
    if (j.p < j.end) { jfail(&j, "unexpected text after the value"); jfree(&root); r = json_err(j.err); }
    else {
      JC c; d_buf_init(&c.path); buf_ch(&c.path, '$'); c.err[0] = 0;
      V x;
      r = jconv(&c, &root, ty, &x) ? d_ok(x) : json_err(c.err);
      free(c.path.p);
      jfree(&root);
    }
  }
  d_release(text);
  return r;
}

/* ------------------------------------------------------------------ http
   HTTP/1.1 over plain sockets. TLS comes from the system OpenSSL 3, loaded with dlopen the first time
   an https URL is used, so binaries keep no link-time dependency. */

#define HTTP_MAX_HEAD (64 * 1024)
#define HTTP_MAX_BODY ((int64_t)256 << 20)

static struct {
  int tried; char err[200]; void* ctx;
  int (*init_ssl)(uint64_t, const void*);
  const void* (*client_method)(void);
  void* (*ctx_new)(const void*);
  int (*ctx_default_paths)(void*);
  void (*ctx_set_verify)(void*, int, void*);
  long (*ctx_ctrl)(void*, int, long, void*);
  void* (*ssl_new)(void*);
  int (*set_fd)(void*, int);
  long (*ctrl)(void*, int, long, void*);
  int (*set1_host)(void*, const char*);
  int (*connect)(void*);
  int (*read)(void*, void*, int);
  int (*write)(void*, const void*, int);
  int (*shutdown)(void*);
  void (*free)(void*);
  int (*get_error)(const void*, int);
  long (*verify_result)(const void*);
  const char* (*verify_str)(long);
  unsigned long (*err_get)(void);
  void (*err_str)(unsigned long, char*, size_t);
} tls;

static int tls_load(void) {
  if (tls.tried) return tls.ctx != NULL;
  tls.tried = 1;
  void* ssl = dlopen("libssl.so.3", RTLD_NOW | RTLD_GLOBAL);
  void* cry = dlopen("libcrypto.so.3", RTLD_NOW | RTLD_GLOBAL);
  if (!ssl || !cry) { snprintf(tls.err, sizeof tls.err, "https needs OpenSSL 3 (libssl.so.3): %s", dlerror()); return 0; }
#define SYM(h, field, name) if (!(*(void**)&tls.field = dlsym(h, name))) { snprintf(tls.err, sizeof tls.err, "OpenSSL is missing %s", name); return 0; }
  SYM(ssl, init_ssl, "OPENSSL_init_ssl") SYM(ssl, client_method, "TLS_client_method") SYM(ssl, ctx_new, "SSL_CTX_new")
  SYM(ssl, ctx_default_paths, "SSL_CTX_set_default_verify_paths") SYM(ssl, ctx_set_verify, "SSL_CTX_set_verify")
  SYM(ssl, ctx_ctrl, "SSL_CTX_ctrl") SYM(ssl, ssl_new, "SSL_new") SYM(ssl, set_fd, "SSL_set_fd") SYM(ssl, ctrl, "SSL_ctrl")
  SYM(ssl, set1_host, "SSL_set1_host") SYM(ssl, connect, "SSL_connect") SYM(ssl, read, "SSL_read") SYM(ssl, write, "SSL_write")
  SYM(ssl, shutdown, "SSL_shutdown") SYM(ssl, free, "SSL_free") SYM(ssl, get_error, "SSL_get_error")
  SYM(ssl, verify_result, "SSL_get_verify_result") SYM(cry, verify_str, "X509_verify_cert_error_string")
  SYM(cry, err_get, "ERR_get_error") SYM(cry, err_str, "ERR_error_string_n")
#undef SYM
  tls.init_ssl(0, NULL);
  void* ctx = tls.ctx_new(tls.client_method());
  if (!ctx || tls.ctx_default_paths(ctx) != 1) { snprintf(tls.err, sizeof tls.err, "cannot create a TLS context"); return 0; }
  tls.ctx_set_verify(ctx, 1 /* SSL_VERIFY_PEER */, NULL);
  tls.ctx_ctrl(ctx, 123 /* SSL_CTRL_SET_MIN_PROTO_VERSION */, 0x0303 /* TLS 1.2 */, NULL);
  tls.ctx = ctx;
  return 1;
}

typedef struct Conn { int fd; void* ssl; char* buf; int64_t pos, len; } Conn;

static void conn_close(Conn* c) {
  if (c->ssl) { tls.shutdown(c->ssl); tls.free(c->ssl); c->ssl = NULL; }
  if (c->fd >= 0) close(c->fd);
  c->fd = -1;
  free(c->buf); c->buf = NULL;
}
static int64_t conn_raw_read(Conn* c, char* p, int64_t n) {
  if (c->ssl) {
    int r = tls.read(c->ssl, p, n > (1 << 30) ? (1 << 30) : (int)n);
    if (r > 0) return r;
    int e = tls.get_error(c->ssl, r);
    return e == 6 /* SSL_ERROR_ZERO_RETURN */ ? 0 : -1;
  }
  for (;;) {
    ssize_t r = recv(c->fd, p, (size_t)n, 0);
    if (r < 0 && errno == EINTR) continue;
    return r;
  }
}
static int conn_write(Conn* c, const char* p, int64_t n) {
  while (n > 0) {
    int64_t w;
    if (c->ssl) { int r = tls.write(c->ssl, p, n > (1 << 30) ? (1 << 30) : (int)n); w = r > 0 ? r : -1; }
    else { w = send(c->fd, p, (size_t)n, MSG_NOSIGNAL); if (w < 0 && errno == EINTR) continue; }
    if (w <= 0) return 0;
    p += w; n -= w;
  }
  return 1;
}
/* buffered reads; return -1 on error, 0 on EOF */
static int64_t conn_fill(Conn* c) {
  if (!c->buf) c->buf = xmalloc(16384);
  if (c->pos < c->len) return c->len - c->pos;
  c->pos = 0;
  c->len = conn_raw_read(c, c->buf, 16384);
  if (c->len < 0) { c->len = 0; return -1; }
  return c->len;
}
/* one line without CRLF; 1 ok, 0 EOF before any byte, -1 error/too long */
static int conn_line(Conn* c, DBuf* out, int64_t limit) {
  out->len = 0;
  int any = 0;
  for (;;) {
    int64_t n = conn_fill(c);
    if (n < 0) return -1;
    if (n == 0) return any ? 1 : 0;
    any = 1;
    char* s = c->buf + c->pos;
    char* nl = memchr(s, '\n', (size_t)n);
    int64_t take = nl ? nl - s + 1 : n;
    d_buf_add(out, s, take);
    c->pos += take;
    if (out->len > limit) return -1;
    if (nl) {
      out->len--;
      if (out->len && out->p[out->len - 1] == '\r') out->len--;
      return 1;
    }
  }
}
static int conn_read_n(Conn* c, DBuf* out, int64_t n) {
  while (n > 0) {
    int64_t a = conn_fill(c);
    if (a <= 0) return 0;
    int64_t t = a < n ? a : n;
    d_buf_add(out, c->buf + c->pos, t);
    c->pos += t; n -= t;
  }
  return 1;
}

static int connect_timeout(int fd, const struct sockaddr* a, socklen_t al, int ms) {
  int fl = fcntl(fd, F_GETFL, 0);
  fcntl(fd, F_SETFL, fl | O_NONBLOCK);
  int r = connect(fd, a, al);
  if (r < 0 && errno == EINPROGRESS) {
    struct pollfd p = {fd, POLLOUT, 0};
    r = poll(&p, 1, ms);
    if (r == 1) { int e = 0; socklen_t el = sizeof e; getsockopt(fd, SOL_SOCKET, SO_ERROR, &e, &el); errno = e; r = e ? -1 : 0; }
    else { if (r == 0) errno = ETIMEDOUT; r = -1; }
  }
  fcntl(fd, F_SETFL, fl);
  return r;
}
static void set_timeouts(int fd, int sec) {
  struct timeval tv = {sec, 0};
  setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv);
  setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof tv);
}

typedef struct Url { int https; char host[256]; char port[8]; char* target; } Url;
static int url_parse(const char* u, Url* out, char* err, size_t en) {
  const char* p;
  if (!strncmp(u, "http://", 7)) { out->https = 0; p = u + 7; }
  else if (!strncmp(u, "https://", 8)) { out->https = 1; p = u + 8; }
  else { snprintf(err, en, "unsupported URL (want http:// or https://): %.100s", u); return 0; }
  const char* end = p + strcspn(p, "/?#");
  const char* at = memchr(p, '@', (size_t)(end - p));
  if (at) { snprintf(err, en, "credentials in URLs are not supported: %.100s", u); return 0; }
  const char* hs = p; const char* he; const char* port = NULL;
  if (*p == '[') { hs = p + 1; he = memchr(hs, ']', (size_t)(end - hs)); if (!he) { snprintf(err, en, "bad IPv6 host in %.100s", u); return 0; } if (he + 1 < end && he[1] == ':') port = he + 2; }
  else { he = memchr(p, ':', (size_t)(end - p)); if (he) port = he + 1; else he = end; }
  if (he == hs || he - hs >= (int64_t)sizeof out->host) { snprintf(err, en, "bad host in URL %.100s", u); return 0; }
  memcpy(out->host, hs, (size_t)(he - hs)); out->host[he - hs] = 0;
  if (port) {
    if (end - port < 1 || end - port > 5) { snprintf(err, en, "bad port in URL %.100s", u); return 0; }
    for (const char* q = port; q < end; q++) if (*q < '0' || *q > '9') { snprintf(err, en, "bad port in URL %.100s", u); return 0; }
    memcpy(out->port, port, (size_t)(end - port)); out->port[end - port] = 0;
  } else strcpy(out->port, out->https ? "443" : "80");
  const char* hash = strchr(end, '#');
  size_t tl = hash ? (size_t)(hash - end) : strlen(end);
  out->target = xmalloc(tl + 2);
  if (tl == 0 || *end == '?') { out->target[0] = '/'; memcpy(out->target + 1, end, tl); out->target[tl + 1] = 0; }
  else { memcpy(out->target, end, tl); out->target[tl] = 0; }
  return 1;
}

static int http_connect(Url* u, Conn* c, char* err, size_t en) {
  memset(c, 0, sizeof *c); c->fd = -1;
  if (u->https && !tls_load()) { snprintf(err, en, "%s", tls.err); return 0; }
  struct addrinfo hints, *res = NULL;
  memset(&hints, 0, sizeof hints);
  hints.ai_family = AF_UNSPEC; hints.ai_socktype = SOCK_STREAM;
  int g = getaddrinfo(u->host, u->port, &hints, &res);
  if (g) { snprintf(err, en, "cannot resolve %s: %s", u->host, gai_strerror(g)); return 0; }
  int fd = -1, last = 0;
  for (struct addrinfo* a = res; a; a = a->ai_next) {
    fd = socket(a->ai_family, a->ai_socktype | SOCK_CLOEXEC, a->ai_protocol);
    if (fd < 0) { last = errno; continue; }
    if (connect_timeout(fd, a->ai_addr, a->ai_addrlen, 15000) == 0) break;
    last = errno; close(fd); fd = -1;
  }
  freeaddrinfo(res);
  if (fd < 0) { snprintf(err, en, "cannot connect to %s:%s: %s", u->host, u->port, strerror(last)); return 0; }
  set_timeouts(fd, 30);
  int one = 1; setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
  c->fd = fd;
  if (u->https) {
    c->ssl = tls.ssl_new(tls.ctx);
    tls.set_fd(c->ssl, fd);
    tls.ctrl(c->ssl, 55 /* SSL_CTRL_SET_TLSEXT_HOSTNAME */, 0, u->host);
    tls.set1_host(c->ssl, u->host);
    if (tls.connect(c->ssl) != 1) {
      long v = tls.verify_result(c->ssl);
      if (v) snprintf(err, en, "TLS certificate check failed for %s: %s", u->host, tls.verify_str(v));
      else { char e[160] = "handshake failed"; unsigned long x = tls.err_get(); if (x) tls.err_str(x, e, sizeof e); snprintf(err, en, "TLS with %s failed: %s", u->host, e); }
      conn_close(c);
      return 0;
    }
  }
  return 1;
}

static V http_error(const char* msg, int64_t status) {
  V f[2] = {str_from(K_STR, msg, (int64_t)strlen(msg)), d_int(status)};
  return d_err(d_adt(&d_td_http_HttpError, 0, 2, f));
}
static void lower_ascii(char* p, int64_t n) { for (int64_t i = 0; i < n; i++) if (p[i] >= 'A' && p[i] <= 'Z') p[i] += 32; }
static V cstr_v(const char* s) { return str_from(K_STR, s, (int64_t)strlen(s)); }
/* headers map: lowercase name -> value; repeated headers are joined with ", " */
static void header_add(V m, const char* name, int64_t nl, const char* val, int64_t vl) {
  V k = str_from(K_STR, name, nl);
  lower_ascii(S(k)->p, nl);
  while (vl && (*val == ' ' || *val == '\t')) { val++; vl--; }
  while (vl && (val[vl - 1] == ' ' || val[vl - 1] == '\t')) vl--;
  V* slot = map_lookup(M(m), k);
  if (slot) {
    DBuf b; d_buf_init(&b);
    d_buf_add(&b, S(*slot)->p, S(*slot)->len); d_buf_add(&b, ", ", 2); d_buf_add(&b, val, vl);
    d_release(*slot); *slot = d_buf_done(&b);
    d_release(k);
  } else map_put(M(m), k, str_from(K_STR, val, vl));
}
static const char* map_cget(V m, const char* key) {
  V k = cstr_v(key);
  V* slot = map_lookup(M(m), k);
  d_release(k);
  return slot ? S(*slot)->p : NULL;
}

/* one exchange; on success *resp is a Response and *location the redirect target (or NULL) */
static int http_once(const char* method, Url* u, V extra, const char* body, int64_t blen, V* resp, char* err, size_t en) {
  Conn c;
  if (!http_connect(u, &c, err, en)) return 0;
  DBuf q; d_buf_init(&q);
  buf_fmt(&q, "%s %s HTTP/1.1\r\nHost: ", method, u->target);
  int v6 = strchr(u->host, ':') != NULL;
  if (v6) buf_ch(&q, '[');
  buf_cstr(&q, u->host);
  if (v6) buf_ch(&q, ']');
  if (strcmp(u->port, u->https ? "443" : "80")) buf_fmt(&q, ":%s", u->port);
  buf_cstr(&q, "\r\n");
  int has_ua = 0, has_ct = 0;
  if (extra.k == K_MAP) {
    DMap* m = M(extra);
    for (int64_t i = 0; i < m->n; i++) {
      if (!m->e[i].live) continue;
      DStr *k = S(m->e[i].key), *v = S(m->e[i].val);
      if (memchr(k->p, '\n', (size_t)k->len) || memchr(k->p, '\r', (size_t)k->len) || memchr(v->p, '\n', (size_t)v->len) || memchr(v->p, '\r', (size_t)v->len)) {
        snprintf(err, en, "header %.60s contains a line break", k->p); free(q.p); conn_close(&c); return 0;
      }
      if (k->len == 10 && !strncasecmp(k->p, "user-agent", 10)) has_ua = 1;
      if (k->len == 12 && !strncasecmp(k->p, "content-type", 12)) has_ct = 1;
      if ((k->len == 14 && !strncasecmp(k->p, "content-length", 14)) || (k->len == 4 && !strncasecmp(k->p, "host", 4)) || (k->len == 10 && !strncasecmp(k->p, "connection", 10))) continue;
      d_buf_add(&q, k->p, k->len); buf_cstr(&q, ": "); d_buf_add(&q, v->p, v->len); buf_cstr(&q, "\r\n");
    }
  }
  if (!has_ua) buf_cstr(&q, "User-Agent: drift/0.4\r\n");
  buf_cstr(&q, "Accept: */*\r\nConnection: close\r\n");
  int sends_body = blen > 0 || (strcmp(method, "GET") && strcmp(method, "HEAD"));
  if (sends_body) {
    if (!has_ct && blen) buf_cstr(&q, "Content-Type: text/plain; charset=utf-8\r\n");
    buf_fmt(&q, "Content-Length: %lld\r\n", (long long)blen);
  }
  buf_cstr(&q, "\r\n");
  int ok = conn_write(&c, q.p, q.len) && (!blen || conn_write(&c, body, blen));
  free(q.p);
  if (!ok) { snprintf(err, en, "sending the request to %s failed: %s", u->host, strerror(errno)); conn_close(&c); return 0; }
  DBuf line; d_buf_init(&line);
  int64_t status = 0;
  V headers = map_new(K_MAP, 16);
  DBuf bodyb; d_buf_init(&bodyb);
  int r;
  /* status line, skipping 1xx interim responses */
  for (;;) {
    r = conn_line(&c, &line, HTTP_MAX_HEAD);
    buf_ch(&line, 0); line.len--;
    if (r <= 0 || line.len < 12 || strncmp(line.p, "HTTP/1.", 7)) { snprintf(err, en, r < 0 && errno == EAGAIN ? "%s timed out" : "%s sent no valid HTTP response", u->host); goto fail; }
    status = strtol(line.p + 9, NULL, 10);
    if (status < 100 || status > 999) { snprintf(err, en, "%s sent a bad status line", u->host); goto fail; }
    int64_t total = 0;
    for (;;) {
      r = conn_line(&c, &line, HTTP_MAX_HEAD);
      if (r <= 0) { snprintf(err, en, "%s closed the connection inside the headers", u->host); goto fail; }
      if (line.len == 0) break;
      total += line.len;
      if (total > HTTP_MAX_HEAD) { snprintf(err, en, "%s sent headers over 64 KiB", u->host); goto fail; }
      char* colon = memchr(line.p, ':', (size_t)line.len);
      if (colon && status >= 200) header_add(headers, line.p, colon - line.p, colon + 1, line.len - (colon + 1 - line.p));
    }
    if (status >= 200) break;
  }
  if (strcmp(method, "HEAD") && status != 204 && status != 304) {
    const char* te = map_cget(headers, "transfer-encoding");
    const char* cl = map_cget(headers, "content-length");
    if (te && strstr(te, "chunked")) {
      for (;;) {
        if (conn_line(&c, &line, 1024) <= 0) { snprintf(err, en, "%s: broken chunked body", u->host); goto fail; }
        buf_ch(&line, 0);
        char* e2; long long n = strtoll(line.p, &e2, 16);
        if (e2 == line.p || n < 0 || bodyb.len + n > HTTP_MAX_BODY) { snprintf(err, en, "%s: bad chunk size", u->host); goto fail; }
        if (n == 0) break;
        if (!conn_read_n(&c, &bodyb, n) || conn_line(&c, &line, 16) <= 0) { snprintf(err, en, "%s: truncated chunked body", u->host); goto fail; }
      }
    } else if (cl) {
      long long n = strtoll(cl, NULL, 10);
      if (n < 0 || n > HTTP_MAX_BODY) { snprintf(err, en, "%s: body too large (%lld bytes)", u->host, n); goto fail; }
      if (!conn_read_n(&c, &bodyb, n)) { snprintf(err, en, "%s: body truncated", u->host); goto fail; }
    } else {
      for (;;) {
        int64_t a = conn_fill(&c);
        if (a < 0) { snprintf(err, en, "%s: reading the body failed", u->host); goto fail; }
        if (a == 0) break;
        d_buf_add(&bodyb, c.buf + c.pos, a); c.pos += a;
        if (bodyb.len > HTTP_MAX_BODY) { snprintf(err, en, "%s: body over 256 MiB", u->host); goto fail; }
      }
    }
  }
  conn_close(&c);
  free(line.p);
  V f[3] = {d_int(status), headers, buf_done_kind(&bodyb, K_BYTES)};
  *resp = d_adt(&d_td_http_Response, 0, 3, f);
  return 1;
fail:
  conn_close(&c);
  free(line.p); free(bodyb.p);
  d_release(headers);
  return 0;
}

static char* resolve_location(const Url* base, const char* loc) {
  DBuf b; d_buf_init(&b);
  if (!strncmp(loc, "http://", 7) || !strncmp(loc, "https://", 8)) buf_cstr(&b, loc);
  else {
    buf_cstr(&b, base->https ? "https://" : "http://");
    if (!strncmp(loc, "//", 2)) buf_cstr(&b, loc + 2);
    else {
      int v6 = strchr(base->host, ':') != NULL;
      buf_fmt(&b, v6 ? "[%s]:%s" : "%s:%s", base->host, base->port);
      if (loc[0] == '/') buf_cstr(&b, loc);
      else {
        const char* slash = strrchr(base->target, '/');
        const char* q = strchr(base->target, '?');
        if (q && slash > q) slash = memrchr(base->target, '/', (size_t)(q - base->target));
        d_buf_add(&b, base->target, slash ? slash - base->target + 1 : 1);
        buf_cstr(&b, loc);
      }
    }
  }
  buf_ch(&b, 0);
  return b.p;
}

static V http_do(const char* method0, V url, V headers, V body, int errors_for_status) {
  char err[400];
  char method[16];
  snprintf(method, sizeof method, "%s", method0);
  char* cur = xmalloc((size_t)S(url)->len + 1);
  memcpy(cur, S(url)->p, (size_t)S(url)->len + 1);
  const char* bp = body.k == K_STR ? S(body)->p : "";
  int64_t bl = body.k == K_STR ? S(body)->len : 0;
  V result;
  for (int hop = 0;; hop++) {
    Url u;
    if (!url_parse(cur, &u, err, sizeof err)) { result = http_error(err, 0); break; }
    V resp;
    int ok = http_once(method, &u, headers, bp, bl, &resp, err, sizeof err);
    if (!ok) { free(u.target); result = http_error(err, 0); break; }
    int64_t st = A(resp)->f[0].i;
    const char* loc = (st == 301 || st == 302 || st == 303 || st == 307 || st == 308) ? map_cget(A(resp)->f[1], "location") : NULL;
    if (loc && hop < 10) {
      char* next = resolve_location(&u, loc);
      free(u.target); free(cur); cur = next;
      if (st == 303 || ((st == 301 || st == 302) && strcmp(method, "GET") && strcmp(method, "HEAD"))) { strcpy(method, "GET"); bl = 0; }
      d_release(resp);
      continue;
    }
    free(u.target);
    if (errors_for_status && st >= 400) {
      DStr* b = S(A(resp)->f[2]);
      int64_t sn = b->len < 200 ? b->len : 200;
      while (sn > 0 && ((unsigned char)b->p[sn - 1] & 0xc0) == 0x80) sn--;
      snprintf(err, sizeof err, "%s %s: status %lld%s%.*s", method, cur, (long long)st, sn ? ": " : "", (int)sn, b->p);
      for (char* x = err; *x; x++) if (*x == '\n' || *x == '\r') *x = ' ';
      d_release(resp);
      result = http_error(err, st);
      break;
    }
    result = d_ok(resp);
    break;
  }
  free(cur);
  return result;
}
static void http_init_once(void) { static int done; if (!done) { done = 1; signal(SIGPIPE, SIG_IGN); } }
V d_n_http_get(V url) {
  http_init_once();
  V r = http_do("GET", url, D_UNIT, D_UNIT, 1);
  d_release(url);
  return r;
}
V d_n_http_post(V url, V body) {
  http_init_once();
  V r = http_do("POST", url, D_UNIT, body, 1);
  d_release(url); d_release(body);
  return r;
}
V d_n_http_request(V method, V url, V headers, V body) {
  http_init_once();
  V r;
  DStr* m = S(method);
  int okm = m->len > 0 && m->len < 16;
  for (int64_t i = 0; i < m->len && okm; i++) if (m->p[i] < 'A' || m->p[i] > 'Z') okm = 0;
  if (!okm) r = http_error("bad HTTP method (use an upper-case token such as GET or PUT)", 0);
  else r = http_do(m->p, url, headers, body, 0);
  d_release(method); d_release(url); d_release(headers); d_release(body);
  return r;
}
/* charset parameter of a Content-Type value, lower-cased, or "" */
static void ctype_charset(const char* ct, char* out, size_t n) {
  out[0] = 0;
  if (!ct) return;
  const char* p = strcasestr(ct, "charset=");
  if (!p) return;
  p += 8;
  if (*p == '"') p++;
  size_t i = 0;
  while (p[i] && p[i] != ';' && p[i] != '"' && p[i] != ' ' && i + 1 < n) { out[i] = (char)(p[i] >= 'A' && p[i] <= 'Z' ? p[i] + 32 : p[i]); i++; }
  out[i] = 0;
}
/* windows-1252 (what browsers use for iso-8859-1 and latin1 labels) to UTF-8 */
static V cp1252_to_str(const char* p, int64_t n) {
  static const uint16_t hi[32] = {0x20ac, 0x81, 0x201a, 0x192, 0x201e, 0x2026, 0x2020, 0x2021, 0x2c6, 0x2030, 0x160, 0x2039, 0x152, 0x8d, 0x17d, 0x8f,
                                  0x90, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014, 0x2dc, 0x2122, 0x161, 0x203a, 0x153, 0x9d, 0x17e, 0x178};
  DBuf b; d_buf_init(&b);
  for (int64_t i = 0; i < n; i++) {
    unsigned char c = (unsigned char)p[i];
    utf8_put(&b, c >= 0x80 && c < 0xa0 ? hi[c - 0x80] : c);
  }
  return d_buf_done(&b);
}
V d_n_http_Response_text(V r) {
  DStr* b = S(A(r)->f[2]);
  char cs[40];
  ctype_charset(map_cget(A(r)->f[1], "content-type"), cs, sizeof cs);
  V out;
  if (!strcmp(cs, "iso-8859-1") || !strcmp(cs, "latin1") || !strcmp(cs, "windows-1252") || !strcmp(cs, "us-ascii") || !strcmp(cs, "ascii"))
    out = d_ok(cp1252_to_str(b->p, b->len));
  else if (cs[0] && strcmp(cs, "utf-8") && strcmp(cs, "utf8")) {
    char m[120];
    snprintf(m, sizeof m, "response charset `%s` is not supported; use `.body()` for the bytes", cs);
    out = http_error(m, A(r)->f[0].i);
  } else if (utf8_valid(b->p, b->len)) out = d_ok(str_from(K_STR, b->p, b->len));
  else out = http_error("response body is not valid UTF-8; use `.body()` for the bytes", A(r)->f[0].i);
  d_release(r);
  return out;
}
V d_n_http_Response_body(V r) { V b = d_retain(A(r)->f[2]); d_release(r); return b; }
V d_n_http_Response_status(V r) { V s = A(r)->f[0]; d_release(r); return s; }
V d_n_http_Response_headers(V r) { V h = d_retain(A(r)->f[1]); d_release(r); return h; }
V d_n_http_Response_header(V r, V name) {
  DBuf b; d_buf_init(&b); d_buf_add(&b, S(name)->p, S(name)->len); V k = d_buf_done(&b);
  lower_ascii(S(k)->p, S(k)->len);
  V* slot = map_lookup(M(A(r)->f[1]), k);
  V out = slot ? d_some(d_retain(*slot)) : d_none();
  d_release(k); d_release(r); d_release(name);
  return out;
}
static V response_new(int64_t status, const char* ctype, V body) {
  V h = map_new(K_MAP, 4);
  if (ctype) map_put(M(h), cstr_v("content-type"), cstr_v(ctype));
  V f[3] = {d_int(status), h, body};
  return d_adt(&d_td_http_Response, 0, 3, f);
}
static V str_to_bytes(V s) { V b = str_from(K_BYTES, S(s)->p, S(s)->len); d_release(s); return b; }
V d_n_http_Response__json(V body) { return response_new(200, "application/json", str_to_bytes(body)); }
V d_n_http_Response__text(V body) { return response_new(200, "text/plain; charset=utf-8", str_to_bytes(body)); }
V d_n_http_Response__html(V body) { return response_new(200, "text/html; charset=utf-8", str_to_bytes(body)); }
V d_n_http_Response__status(V code) { return response_new(code.i, NULL, str_from(K_BYTES, "", 0)); }
V d_n_http_Response_with_status(V r, V code) {
  adt_unique(&r);
  A(r)->f[0] = code;
  return r;
}
V d_n_http_Response_with_header(V r, V name, V value) {
  if (memchr(S(name)->p, '\n', (size_t)S(name)->len) || memchr(S(name)->p, '\r', (size_t)S(name)->len) ||
      memchr(S(value)->p, '\n', (size_t)S(value)->len) || memchr(S(value)->p, '\r', (size_t)S(value)->len))
    d_panic_c("http header names and values cannot contain line breaks");
  adt_unique(&r);
  map_unique(&A(r)->f[1]);
  V k = str_from(K_STR, S(name)->p, S(name)->len);
  lower_ascii(S(k)->p, S(k)->len);
  map_put(M(A(r)->f[1]), k, value);
  d_release(name);
  return r;
}

/* ---- server */
static const char* reason(int64_t s) {
  switch (s) {
  case 200: return "OK"; case 201: return "Created"; case 202: return "Accepted"; case 204: return "No Content";
  case 301: return "Moved Permanently"; case 302: return "Found"; case 303: return "See Other"; case 304: return "Not Modified";
  case 307: return "Temporary Redirect"; case 308: return "Permanent Redirect";
  case 400: return "Bad Request"; case 401: return "Unauthorized"; case 403: return "Forbidden"; case 404: return "Not Found";
  case 405: return "Method Not Allowed"; case 408: return "Request Timeout"; case 409: return "Conflict"; case 411: return "Length Required";
  case 413: return "Content Too Large"; case 415: return "Unsupported Media Type"; case 422: return "Unprocessable Content";
  case 429: return "Too Many Requests"; case 431: return "Request Header Fields Too Large";
  case 500: return "Internal Server Error"; case 501: return "Not Implemented"; case 502: return "Bad Gateway"; case 503: return "Service Unavailable";
  default: return "Status";
  }
}
static int hexv(char c) { return c >= '0' && c <= '9' ? c - '0' : c >= 'a' && c <= 'f' ? c - 'a' + 10 : c >= 'A' && c <= 'F' ? c - 'A' + 10 : -1; }
/* percent-decoding; `plus` turns '+' into ' ' (query strings). NULL if the result is not UTF-8. */
static V pct_decode(const char* p, int64_t n, int plus) {
  DBuf b; d_buf_init(&b);
  for (int64_t i = 0; i < n; i++) {
    char ch = p[i];
    if (ch == '%' && i + 2 < n && hexv(p[i + 1]) >= 0 && hexv(p[i + 2]) >= 0) { ch = (char)(hexv(p[i + 1]) * 16 + hexv(p[i + 2])); i += 2; }
    else if (plus && ch == '+') ch = ' ';
    buf_ch(&b, ch);
  }
  if (!utf8_valid(b.p, b.len)) { free(b.p); return D_UNIT; }
  return d_buf_done(&b);
}
static void send_simple(Conn* c, int64_t status, const char* msg) {
  DBuf b; d_buf_init(&b);
  buf_fmt(&b, "HTTP/1.1 %lld %s\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: %zu\r\nConnection: close\r\n\r\n%s", (long long)status, reason(status), strlen(msg), msg);
  conn_write(c, b.p, b.len);
  free(b.p);
}
static void serve_one(int fd, V handler) {
  Conn c; memset(&c, 0, sizeof c); c.fd = fd;
  set_timeouts(fd, 10);
  DBuf line; d_buf_init(&line);
  V headers = map_new(K_MAP, 16), query = map_new(K_MAP, 4);
  V method = D_UNIT, path = D_UNIT;
  if (conn_line(&c, &line, 8192) <= 0) goto done;
  char* sp1 = memchr(line.p, ' ', (size_t)line.len);
  char* sp2 = sp1 ? memchr(sp1 + 1, ' ', (size_t)(line.len - (sp1 + 1 - line.p))) : NULL;
  if (!sp1 || !sp2 || sp1 == line.p || sp1[1] != '/') { send_simple(&c, 400, "bad request line\n"); goto done; }
  method = str_from(K_STR, line.p, sp1 - line.p);
  char* target = sp1 + 1; int64_t tl = sp2 - target;
  char* qm = memchr(target, '?', (size_t)tl);
  path = pct_decode(target, qm ? qm - target : tl, 0);
  if (path.k != K_STR) { send_simple(&c, 400, "path is not UTF-8\n"); goto done; }
  if (qm) {
    char* q = qm + 1; char* qe = target + tl;
    while (q < qe) {
      char* amp = memchr(q, '&', (size_t)(qe - q)); if (!amp) amp = qe;
      char* eq = memchr(q, '=', (size_t)(amp - q));
      if (amp > q) {
        V k = pct_decode(q, (eq ? eq : amp) - q, 1);
        V v = eq ? pct_decode(eq + 1, amp - eq - 1, 1) : str_from(K_STR, "", 0);
        if (k.k != K_STR || v.k != K_STR) { d_release(k); d_release(v); send_simple(&c, 400, "query is not UTF-8\n"); goto done; }
        map_put(M(query), k, v);
      }
      q = amp + 1;
    }
  }
  int64_t total = 0;
  for (;;) {
    int r = conn_line(&c, &line, 8192);
    if (r <= 0) { send_simple(&c, 400, "incomplete headers\n"); goto done; }
    if (line.len == 0) break;
    total += line.len;
    if (total > HTTP_MAX_HEAD) { send_simple(&c, 431, "headers too large\n"); goto done; }
    char* colon = memchr(line.p, ':', (size_t)line.len);
    if (colon) header_add(headers, line.p, colon - line.p, colon + 1, line.len - (colon + 1 - line.p));
  }
  DBuf body; d_buf_init(&body);
  const char* te = map_cget(headers, "transfer-encoding");
  const char* cl = map_cget(headers, "content-length");
  if (te) { free(body.p); send_simple(&c, 501, "chunked request bodies are not supported\n"); goto done; }
  if (cl) {
    long long n = strtoll(cl, NULL, 10);
    if (n < 0 || n > ((int64_t)16 << 20)) { free(body.p); send_simple(&c, 413, "body over 16 MiB\n"); goto done; }
    if (!conn_read_n(&c, &body, n)) { free(body.p); goto done; }
  }
  if (!utf8_valid(body.p, body.len)) { free(body.p); send_simple(&c, 415, "body is not UTF-8\n"); goto done; }
  V f[5] = {method, path, query, headers, d_buf_done(&body)};
  method = path = query = headers = D_UNIT;
  V req = d_adt(&d_td_http_Request, 0, 5, f);
  int is_head = !strcmp(S(f[0])->p, "HEAD");
  V resp = call1(handler, req);
  DAdt* ra = A(resp);
  DStr* rb = S(ra->f[2]);
  DBuf out; d_buf_init(&out);
  buf_fmt(&out, "HTTP/1.1 %lld %s\r\n", (long long)ra->f[0].i, reason(ra->f[0].i));
  DMap* hm = M(ra->f[1]);
  for (int64_t i = 0; i < hm->n; i++) {
    if (!hm->e[i].live) continue;
    DStr *k = S(hm->e[i].key), *v = S(hm->e[i].val);
    if ((k->len == 14 && !memcmp(k->p, "content-length", 14)) || (k->len == 10 && !memcmp(k->p, "connection", 10))) continue;
    d_buf_add(&out, k->p, k->len); buf_cstr(&out, ": "); d_buf_add(&out, v->p, v->len); buf_cstr(&out, "\r\n");
  }
  buf_fmt(&out, "Content-Length: %lld\r\nConnection: close\r\n\r\n", (long long)rb->len);
  if (conn_write(&c, out.p, out.len) && !is_head) conn_write(&c, rb->p, rb->len);
  free(out.p);
  d_release(resp);
done:
  free(line.p);
  d_release(headers); d_release(query); d_release(method); d_release(path);
  shutdown(fd, SHUT_WR);
  conn_close(&c);
}
V d_n_http_serve(V addr, V handler) {
  http_init_once();
  char host[256] = "", port[8] = "";
  DStr* a = S(addr);
  const char* colon = a->len && a->p[0] == '[' ? strstr(a->p, "]:") : strrchr(a->p, ':');
  if (colon && a->p[0] == '[') colon++;
  char err[300];
  if (!colon || a->p + a->len - colon - 1 < 1 || a->p + a->len - colon - 1 > 5) {
    snprintf(err, sizeof err, "bad listen address %.100s (want host:port or :port)", a->p);
    d_release(addr); d_release(handler);
    return http_error(err, 0);
  }
  const char* hs = a->p[0] == '[' ? a->p + 1 : a->p;
  const char* he = a->p[0] == '[' ? colon - 1 : colon;
  snprintf(host, sizeof host, "%.*s", (int)(he - hs), hs);
  snprintf(port, sizeof port, "%s", colon + 1);
  struct addrinfo hints, *res = NULL;
  memset(&hints, 0, sizeof hints);
  hints.ai_family = AF_UNSPEC; hints.ai_socktype = SOCK_STREAM; hints.ai_flags = AI_PASSIVE;
  int g = getaddrinfo(host[0] ? host : NULL, port, &hints, &res);
  if (g) { snprintf(err, sizeof err, "cannot listen on %.100s: %s", a->p, gai_strerror(g)); d_release(addr); d_release(handler); return http_error(err, 0); }
  int fd = -1, last = 0;
  for (struct addrinfo* ai = res; ai; ai = ai->ai_next) {
    fd = socket(ai->ai_family, ai->ai_socktype | SOCK_CLOEXEC, ai->ai_protocol);
    if (fd < 0) { last = errno; continue; }
    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    if (bind(fd, ai->ai_addr, ai->ai_addrlen) == 0 && listen(fd, 128) == 0) break;
    last = errno; close(fd); fd = -1;
  }
  freeaddrinfo(res);
  if (fd < 0) { snprintf(err, sizeof err, "cannot listen on %.100s: %s", a->p, strerror(last)); d_release(addr); d_release(handler); return http_error(err, 0); }
  d_release(addr);
  fflush(stdout);
  for (;;) {
    int cfd = accept4(fd, NULL, NULL, SOCK_CLOEXEC);
    if (cfd < 0) { if (errno == EINTR || errno == ECONNABORTED || errno == EMFILE || errno == ENFILE) continue; break; }
    serve_one(cfd, handler);
    fflush(stdout);
  }
  snprintf(err, sizeof err, "accept failed: %s", strerror(errno));
  close(fd);
  d_release(handler);
  return http_error(err, 0);
}

/* ------------------------------------------------------------------ re
   pattern -> AST -> instructions (stored flat in a Bytes value) -> Pike VM over UTF-8.
   Linear in the text: every position is visited once by at most one thread per instruction. */

enum { R_CHAR, R_ANY, R_CLASS, R_SPLIT, R_JMP, R_SAVE, R_ASSERT, R_MATCH };
enum { A_BOL, A_EOL, A_MBOL, A_MEOL, A_TSTART, A_TEND, A_WORDB, A_NWORDB };
/* special class entries (lo field) */
#define RC_SPEC 0xFFFFFF00u
enum { RS_DIGIT, RS_NDIGIT, RS_WORD, RS_NWORD, RS_SPACE, RS_NSPACE };
#define RE_MAX_INST 20000
#define RE_MAX_REPEAT 1000

typedef struct RInst { int32_t op, x, y, z; } RInst;
typedef struct RHead { int32_t ninst, ncap, nrange, pad; } RHead;

static uint32_t fold_lower(uint32_t c) {
  if (c >= 'A' && c <= 'Z') return c + 32;
  if (c < 0xC0) return c;
  if (c <= 0xDE && c != 0xD7) return c + 32;
  if (c >= 0x100 && c <= 0x17F) {
    if (c == 0x178) return 0xFF;
    if ((c <= 0x137 || (c >= 0x14A && c <= 0x177)) && !(c & 1)) return c + 1;
    if (((c >= 0x139 && c <= 0x148) || (c >= 0x179 && c <= 0x17E)) && (c & 1)) return c + 1;
    return c;
  }
  if (c >= 0x391 && c <= 0x3A9 && c != 0x3A2) return c + 32;
  if (c >= 0x410 && c <= 0x42F) return c + 32;
  if (c >= 0x400 && c <= 0x40F) return c + 80;
  return c;
}
static int re_word(uint32_t c) { return is_alnum(c) || c == '_'; }

/* ---- parser */
enum { N_EMPTY, N_CHAR, N_ANY, N_CLASS, N_CAT, N_ALT, N_REP, N_GROUP, N_ASSERT };
typedef struct RNode { int t; uint32_t c; int fold, dotnl, lo, n, neg, min, max, lazy, cap, a, b; } RNode;
typedef struct RP {
  const char* p; int64_t len, i;
  RNode* nodes; int nn, cn;
  uint32_t* rg; int nr, cr;      /* class ranges: pairs */
  int ncap;
  char err[200];
} RP;
static int rp_fail(RP* r, const char* msg) {
  if (!r->err[0]) snprintf(r->err, sizeof r->err, "%s at position %lld", msg, (long long)chars_before(r->p, r->i));
  return -1;
}
static int rnode(RP* r, int t) {
  if (r->nn == r->cn) { r->cn = r->cn ? r->cn * 2 : 32; r->nodes = xrealloc(r->nodes, (size_t)r->cn * sizeof(RNode)); }
  memset(&r->nodes[r->nn], 0, sizeof(RNode));
  r->nodes[r->nn].t = t;
  r->nodes[r->nn].a = r->nodes[r->nn].b = -1;
  return r->nn++;
}
static void rrange(RP* r, uint32_t lo, uint32_t hi) {
  if (r->nr + 2 > r->cr) { r->cr = r->cr ? r->cr * 2 : 32; r->rg = xrealloc(r->rg, (size_t)r->cr * sizeof(uint32_t)); }
  r->rg[r->nr++] = lo; r->rg[r->nr++] = hi;
}
static int rp_more(RP* r) { return r->i < r->len; }
static uint32_t rp_peek(RP* r, int* l) { return utf8_at(r->p, r->len, r->i, l); }
static uint32_t rp_next(RP* r) { int l; uint32_t c = rp_peek(r, &l); r->i += l; return c; }
static int hexdig(uint32_t c) { return c >= '0' && c <= '9' ? (int)c - '0' : c >= 'a' && c <= 'f' ? (int)c - 'a' + 10 : c >= 'A' && c <= 'F' ? (int)c - 'A' + 10 : -1; }

/* after a backslash: a literal codepoint (>= 0), a special class (-2 - RS_*), an assertion (-100 - A_*), or -1 */
static int64_t rp_escape(RP* r, int in_class) {
  if (!rp_more(r)) return rp_fail(r, "trailing backslash");
  int64_t at = r->i;
  uint32_t c = rp_next(r);
  switch (c) {
  case 'd': return -2 - RS_DIGIT; case 'D': return -2 - RS_NDIGIT;
  case 'w': return -2 - RS_WORD; case 'W': return -2 - RS_NWORD;
  case 's': return -2 - RS_SPACE; case 'S': return -2 - RS_NSPACE;
  case 'n': return '\n'; case 't': return '\t'; case 'r': return '\r'; case 'f': return '\f'; case 'v': return '\v'; case '0': return 0;
  case 'x': {
    uint32_t v = 0; int k = 0;
    if (rp_more(r) && r->p[r->i] == '{') {
      r->i++;
      while (rp_more(r) && r->p[r->i] != '}' && k < 7) { int h = hexdig((unsigned char)r->p[r->i]); if (h < 0) return rp_fail(r, "bad hex digit in \\x{...}"); v = v * 16 + (uint32_t)h; r->i++; k++; }
      if (!rp_more(r) || r->p[r->i] != '}' || !k || v > 0x10FFFF) return rp_fail(r, "bad \\x{...} escape");
      r->i++;
    } else {
      for (; k < 2; k++) { int h = rp_more(r) ? hexdig((unsigned char)r->p[r->i]) : -1; if (h < 0) return rp_fail(r, "\\x needs two hex digits or \\x{...}"); v = v * 16 + (uint32_t)h; r->i++; }
    }
    return v;
  }
  }
  if (!in_class) {
    switch (c) {
    case 'b': return -100 - A_WORDB; case 'B': return -100 - A_NWORDB;
    case 'A': return -100 - A_TSTART; case 'z': return -100 - A_TEND;
    }
  }
  if (c >= '1' && c <= '9') { r->i = at; return rp_fail(r, "backreferences are not supported (Drift regexes run in linear time)"); }
  if (c < 0x80 && (isalnum((int)c))) { r->i = at - 1; return rp_fail(r, "unknown escape"); }
  return c; /* escaped punctuation or any non-ASCII character */
}
static void add_spec(RP* r, int spec) { rrange(r, RC_SPEC + (uint32_t)spec, 0); }
static int rp_alt(RP* r, int fold, int dotnl, int multi, int depth);
static int rp_class(RP* r, int fold) {
  int n = rnode(r, N_CLASS);
  r->i++; /* [ */
  int neg = 0;
  if (rp_more(r) && r->p[r->i] == '^') { neg = 1; r->i++; }
  int start = r->nr, first = 1;
  for (;;) {
    if (!rp_more(r)) return rp_fail(r, "missing `]`");
    uint32_t c = (unsigned char)r->p[r->i];
    if (c == ']' && !first) { r->i++; break; }
    first = 0;
    if (c == '[' && r->i + 1 < r->len && r->p[r->i + 1] == ':') return rp_fail(r, "POSIX classes like [:alpha:] are not supported; use \\d, \\w, \\s or ranges");
    int64_t lo;
    if (c == '\\') { r->i++; lo = rp_escape(r, 1); if (lo == -1) return -1; if (lo < -1) { add_spec(r, (int)(-2 - lo)); continue; } }
    else lo = rp_next(r);
    int64_t hi = lo;
    if (r->i + 1 < r->len && r->p[r->i] == '-' && r->p[r->i + 1] != ']') {
      r->i++;
      if (r->p[r->i] == '\\') { r->i++; hi = rp_escape(r, 1); if (hi == -1) return -1; if (hi < 0) return rp_fail(r, "a class like \\d cannot end a range"); }
      else hi = rp_next(r);
      if (hi < lo) return rp_fail(r, "range out of order");
    }
    rrange(r, (uint32_t)lo, (uint32_t)hi);
  }
  if (fold) {
    /* add the lower-case forms so matching can compare fold_lower(c) */
    int end = r->nr;
    for (int k = start; k < end; k += 2) {
      uint32_t lo = r->rg[k], hi = r->rg[k + 1];
      if (lo >= RC_SPEC) continue;
      if (hi - lo > 4096) { rrange(r, fold_lower(lo), fold_lower(lo)); continue; }
      for (uint32_t x = lo; x <= hi; x++) { uint32_t f = fold_lower(x); if (f != x) rrange(r, f, f); }
    }
  }
  RNode* nd = &r->nodes[n];
  nd->lo = start; nd->n = (r->nr - start) / 2; nd->neg = neg; nd->fold = fold;
  return n;
}
static int rp_flags(RP* r, int* fold, int* dotnl, int* multi) {
  int on = 1;
  while (rp_more(r) && r->p[r->i] != ')' && r->p[r->i] != ':') {
    char f = r->p[r->i++];
    if (f == '-') { on = 0; continue; }
    if (f == 'i') *fold = on; else if (f == 's') *dotnl = on; else if (f == 'm') *multi = on;
    else { r->i--; return rp_fail(r, "unknown flag (use i, m or s)"); }
  }
  return 0;
}
static int rp_atom(RP* r, int* fold, int* dotnl, int* multi, int depth) {
  int l;
  uint32_t c = rp_peek(r, &l);
  int n;
  switch (c) {
  case '(': {
    if (depth > 200) return rp_fail(r, "groups nested too deeply");
    r->i++;
    int cap = 1, f = *fold, d = *dotnl, m = *multi;
    if (rp_more(r) && r->p[r->i] == '?') {
      r->i++;
      if (!rp_more(r)) return rp_fail(r, "unfinished group");
      char k = r->p[r->i];
      if (k == '=' || k == '!' || (k == '<' && r->i + 1 < r->len && (r->p[r->i + 1] == '=' || r->p[r->i + 1] == '!')))
        return rp_fail(r, "lookaround is not supported (Drift regexes run in linear time)");
      if (k == 'P' || k == '<') return rp_fail(r, "named groups are not supported; use (...) and match.groups[i]");
      if (k == ':') { r->i++; cap = 0; }
      else {
        if (rp_flags(r, &f, &d, &m) < 0) return -1;
        if (!rp_more(r)) return rp_fail(r, "missing `)`");
        if (r->p[r->i] == ')') { r->i++; *fold = f; *dotnl = d; *multi = m; return rnode(r, N_EMPTY); }
        r->i++; /* ':' */
        cap = 0;
      }
    }
    int idx = cap ? ++r->ncap : 0;
    int inner = rp_alt(r, f, d, m, depth + 1);
    if (inner < 0) return -1;
    if (!rp_more(r) || r->p[r->i] != ')') return rp_fail(r, "missing `)`");
    r->i++;
    n = rnode(r, N_GROUP);
    r->nodes[n].a = inner; r->nodes[n].cap = idx;
    return n;
  }
  case '[': return rp_class(r, *fold);
  case '.': r->i++; n = rnode(r, N_ANY); r->nodes[n].dotnl = *dotnl; return n;
  case '^': r->i++; n = rnode(r, N_ASSERT); r->nodes[n].c = *multi ? A_MBOL : A_BOL; return n;
  case '$': r->i++; n = rnode(r, N_ASSERT); r->nodes[n].c = *multi ? A_MEOL : A_EOL; return n;
  case '*': case '+': case '?': return rp_fail(r, "nothing to repeat");
  case ')': return rp_fail(r, "unmatched `)`");
  case '\\': {
    r->i++;
    int64_t e = rp_escape(r, 0);
    if (e == -1) return -1;
    if (e <= -100) { n = rnode(r, N_ASSERT); r->nodes[n].c = (uint32_t)(-100 - e); return n; }
    if (e < -1) { n = rnode(r, N_CLASS); r->nodes[n].lo = r->nr; r->nodes[n].n = 1; add_spec(r, (int)(-2 - e)); return n; }
    n = rnode(r, N_CHAR); r->nodes[n].c = *fold ? fold_lower((uint32_t)e) : (uint32_t)e; r->nodes[n].fold = *fold;
    return n;
  }
  default:
    r->i += l;
    n = rnode(r, N_CHAR); r->nodes[n].c = *fold ? fold_lower(c) : c; r->nodes[n].fold = *fold;
    return n;
  }
}
static int rp_int(RP* r, int* out) {
  int v = 0, k = 0;
  while (rp_more(r) && r->p[r->i] >= '0' && r->p[r->i] <= '9') { v = v * 10 + (r->p[r->i] - '0'); if (v > 100000) v = 100000; r->i++; k++; }
  *out = v;
  return k;
}
static int rp_repeat(RP* r, int atom) {
  for (;;) {
    if (!rp_more(r)) return atom;
    char c = r->p[r->i];
    int min, max;
    if (c == '*') { min = 0; max = -1; r->i++; }
    else if (c == '+') { min = 1; max = -1; r->i++; }
    else if (c == '?') { min = 0; max = 1; r->i++; }
    else if (c == '{') {
      int64_t save = r->i;
      r->i++;
      if (!rp_int(r, &min)) { r->i = save; return atom; } /* a literal `{` */
      max = min;
      if (rp_more(r) && r->p[r->i] == ',') { r->i++; if (!rp_int(r, &max)) max = -1; }
      if (!rp_more(r) || r->p[r->i] != '}') return rp_fail(r, "bad repetition {n,m}");
      r->i++;
      if (min > RE_MAX_REPEAT || max > RE_MAX_REPEAT) return rp_fail(r, "repetition count over 1000");
      if (max != -1 && max < min) return rp_fail(r, "repetition {n,m} with m < n");
    } else return atom;
    int t = r->nodes[atom].t;
    if (t == N_ASSERT || t == N_EMPTY) return rp_fail(r, "nothing to repeat");
    int lazy = 0;
    if (rp_more(r) && r->p[r->i] == '?') { lazy = 1; r->i++; }
    else if (rp_more(r) && r->p[r->i] == '+') return rp_fail(r, "possessive quantifiers are not supported");
    int n = rnode(r, N_REP);
    r->nodes[n].a = atom; r->nodes[n].min = min; r->nodes[n].max = max; r->nodes[n].lazy = lazy;
    atom = n;
  }
}
static int rp_cat(RP* r, int fold, int dotnl, int multi, int depth) {
  int acc = -1;
  while (rp_more(r) && r->p[r->i] != '|' && r->p[r->i] != ')') {
    int a = rp_atom(r, &fold, &dotnl, &multi, depth);
    if (a < 0) return -1;
    a = rp_repeat(r, a);
    if (a < 0) return -1;
    if (acc < 0) acc = a;
    else { int n = rnode(r, N_CAT); r->nodes[n].a = acc; r->nodes[n].b = a; acc = n; }
  }
  return acc < 0 ? rnode(r, N_EMPTY) : acc;
}
static int rp_alt(RP* r, int fold, int dotnl, int multi, int depth) {
  int a = rp_cat(r, fold, dotnl, multi, depth);
  if (a < 0) return -1;
  while (rp_more(r) && r->p[r->i] == '|') {
    r->i++;
    int b = rp_cat(r, fold, dotnl, multi, depth);
    if (b < 0) return -1;
    int n = rnode(r, N_ALT);
    r->nodes[n].a = a; r->nodes[n].b = b;
    a = n;
  }
  return a;
}

/* ---- compiler */
typedef struct RC { RInst* in; int n, cap; int over; RP* r; } RC;
static int remit(RC* c, int op, int x, int y, int z) {
  if (c->n >= RE_MAX_INST) { c->over = 1; return c->n; }
  if (c->n == c->cap) { c->cap = c->cap ? c->cap * 2 : 64; c->in = xrealloc(c->in, (size_t)c->cap * sizeof(RInst)); }
  c->in[c->n] = (RInst){op, x, y, z};
  return c->n++;
}
static void rcomp(RC* c, int ni) {
  if (c->over) return;
  RNode nd = c->r->nodes[ni];
  switch (nd.t) {
  case N_EMPTY: break;
  case N_CHAR: remit(c, R_CHAR, (int32_t)nd.c, nd.fold, 0); break;
  case N_ANY: remit(c, R_ANY, nd.dotnl, 0, 0); break;
  case N_CLASS: remit(c, R_CLASS, nd.lo, nd.n, nd.neg | nd.fold << 1); break;
  case N_ASSERT: remit(c, R_ASSERT, (int32_t)nd.c, 0, 0); break;
  case N_CAT: {
    /* concatenations lean left (one level per atom): walk the spine instead of recursing on it */
    int cnt = 0;
    for (int k = ni; c->r->nodes[k].t == N_CAT; k = c->r->nodes[k].a) cnt++;
    int* right = xmalloc((size_t)cnt * sizeof(int));
    int k = ni;
    for (int j = cnt - 1; j >= 0; j--) { right[j] = c->r->nodes[k].b; k = c->r->nodes[k].a; }
    rcomp(c, k);
    for (int j = 0; j < cnt && !c->over; j++) rcomp(c, right[j]);
    free(right);
    break;
  }
  case N_ALT: {
    int sp = remit(c, R_SPLIT, 0, 0, 0);
    if (c->over) return;
    c->in[sp].x = c->n; rcomp(c, nd.a);
    int j = remit(c, R_JMP, 0, 0, 0);
    if (c->over) return;
    c->in[sp].y = c->n; rcomp(c, nd.b);
    if (c->over) return;
    c->in[j].x = c->n;
    break;
  }
  case N_GROUP:
    if (nd.cap) remit(c, R_SAVE, 2 * nd.cap, 0, 0);
    rcomp(c, nd.a);
    if (nd.cap) remit(c, R_SAVE, 2 * nd.cap + 1, 0, 0);
    break;
  case N_REP: {
    for (int k = 0; k < nd.min; k++) rcomp(c, nd.a);
    if (nd.max == -1) {
      /* L: split body, out; body; jmp L */
      int sp = remit(c, R_SPLIT, 0, 0, 0);
      if (c->over) return;
      int body = c->n;
      rcomp(c, nd.a);
      remit(c, R_JMP, sp, 0, 0);
      if (c->over) return;
      if (nd.lazy) { c->in[sp].x = c->n; c->in[sp].y = body; } else { c->in[sp].x = body; c->in[sp].y = c->n; }
    } else {
      int* pend = xmalloc((size_t)(nd.max - nd.min + 1) * sizeof(int));
      int np = 0;
      for (int k = nd.min; k < nd.max; k++) {
        int sp = remit(c, R_SPLIT, 0, 0, 0);
        if (c->over) { free(pend); return; }
        pend[np++] = sp;
        c->in[sp].x = c->n; /* provisional: body first (greedy) */
        rcomp(c, nd.a);
      }
      if (c->over) { free(pend); return; }
      for (int k = 0; k < np; k++) {
        int sp = pend[k];
        int body = sp + 1;
        if (nd.lazy) { c->in[sp].x = c->n; c->in[sp].y = body; } else { c->in[sp].x = body; c->in[sp].y = c->n; }
      }
      free(pend);
    }
    break;
  }
  }
}

static V regex_error(const char* pattern, const char* msg) {
  char m[400];
  snprintf(m, sizeof m, "invalid regex `%.120s`: %s", pattern, msg);
  V s = cstr(m);
  return d_err(d_adt(&d_td_re_RegexError, 0, 1, &s));
}
V d_n_re_compile(V pat) {
  RP r; memset(&r, 0, sizeof r);
  r.p = S(pat)->p; r.len = S(pat)->len;
  int root = rp_alt(&r, 0, 0, 0, 0);
  if (root >= 0 && rp_more(&r)) root = rp_fail(&r, "unmatched `)`");
  V out;
  if (root < 0) out = regex_error(S(pat)->p, r.err);
  else {
    RC c; memset(&c, 0, sizeof c); c.r = &r;
    remit(&c, R_SAVE, 0, 0, 0);
    rcomp(&c, root);
    remit(&c, R_SAVE, 1, 0, 0);
    remit(&c, R_MATCH, 0, 0, 0);
    if (c.over) out = regex_error(S(pat)->p, "pattern too large (over 20000 instructions after expanding repetitions)");
    else {
      RHead h = {c.n, 2 * (r.ncap + 1), r.nr, 0};
      int64_t sz = (int64_t)sizeof h + (int64_t)c.n * (int64_t)sizeof(RInst) + (int64_t)r.nr * 4;
      V prog = str_alloc(K_BYTES, sz);
      char* q = S(prog)->p;
      memcpy(q, &h, sizeof h);
      memcpy(q + sizeof h, c.in, (size_t)c.n * sizeof(RInst));
      if (r.nr) memcpy(q + sizeof h + (size_t)c.n * sizeof(RInst), r.rg, (size_t)r.nr * 4);
      V f[2] = {d_retain(pat), prog};
      out = d_ok(d_adt(&d_td_re_Regex, 0, 2, f));
    }
    free(c.in);
  }
  free(r.nodes); free(r.rg);
  d_release(pat);
  return out;
}

/* ---- VM */
typedef struct RVM {
  const RHead* h; const RInst* in; const uint32_t* rg;
  const char* s; int64_t n;
  int32_t* mark; int32_t gen;
  int32_t *cpc, *npc; int64_t *ccap, *ncapv; int cn, nn;
  int64_t* tmp;
} RVM;
static RVM* rvm_new(V rx) {
  RVM* m = xmalloc(sizeof(RVM));
  const char* p = S(A(rx)->f[1])->p;
  m->h = (const RHead*)p;
  m->in = (const RInst*)(p + sizeof(RHead));
  m->rg = (const uint32_t*)(p + sizeof(RHead) + (size_t)m->h->ninst * sizeof(RInst));
  int ni = m->h->ninst, nc = m->h->ncap;
  m->mark = calloc((size_t)ni, sizeof(int32_t));
  m->gen = 0;
  m->cpc = xmalloc((size_t)ni * sizeof(int32_t)); m->npc = xmalloc((size_t)ni * sizeof(int32_t));
  m->ccap = xmalloc((size_t)ni * (size_t)nc * sizeof(int64_t)); m->ncapv = xmalloc((size_t)ni * (size_t)nc * sizeof(int64_t));
  m->tmp = xmalloc((size_t)nc * sizeof(int64_t));
  return m;
}
static void rvm_free(RVM* m) { free(m->mark); free(m->cpc); free(m->npc); free(m->ccap); free(m->ncapv); free(m->tmp); free(m); }
static int rclass(const RVM* m, const RInst* ins, uint32_t c) {
  const uint32_t* g = m->rg + ins->x;
  int fold = ins->z >> 1, hit = 0;
  uint32_t fc = fold ? fold_lower(c) : c;
  for (int k = 0; k < ins->y && !hit; k++) {
    uint32_t lo = g[2 * k], hi = g[2 * k + 1];
    if (lo >= RC_SPEC) {
      switch (lo - RC_SPEC) {
      case RS_DIGIT: hit = is_digit(c); break; case RS_NDIGIT: hit = !is_digit(c); break;
      case RS_WORD: hit = re_word(c); break; case RS_NWORD: hit = !re_word(c); break;
      case RS_SPACE: hit = is_ws(c); break; default: hit = !is_ws(c); break;
      }
    } else hit = (c >= lo && c <= hi) || (fold && fc >= lo && fc <= hi);
  }
  return hit != (ins->z & 1);
}
static uint32_t prev_char(const char* s, int64_t sp) {
  if (sp <= 0) return 0xFFFFFFFFu;
  int64_t k = sp - 1;
  while (k > 0 && ((unsigned char)s[k] & 0xC0) == 0x80) k--;
  int l; return utf8_at(s, sp, k, &l);
}
static int rassert(const RVM* m, int kind, int64_t sp) {
  switch (kind) {
  case A_BOL: case A_TSTART: return sp == 0;
  case A_EOL: case A_TEND: return sp == m->n;
  case A_MBOL: return sp == 0 || m->s[sp - 1] == '\n';
  case A_MEOL: return sp == m->n || m->s[sp] == '\n';
  default: {
    uint32_t a = prev_char(m->s, sp);
    int l; uint32_t b = sp < m->n ? utf8_at(m->s, m->n, sp, &l) : 0xFFFFFFFFu;
    int wa = a != 0xFFFFFFFFu && re_word(a), wb = b != 0xFFFFFFFFu && re_word(b);
    return (wa != wb) == (kind == A_WORDB);
  }
  }
}
static void radd(RVM* m, int32_t* pcs, int64_t* caps, int* cnt, int pc, int64_t* cur, int64_t sp) {
  if (m->mark[pc] == m->gen) return;
  m->mark[pc] = m->gen;
  const RInst* ins = &m->in[pc];
  switch (ins->op) {
  case R_JMP: radd(m, pcs, caps, cnt, ins->x, cur, sp); return;
  case R_SPLIT: radd(m, pcs, caps, cnt, ins->x, cur, sp); radd(m, pcs, caps, cnt, ins->y, cur, sp); return;
  case R_SAVE: { int64_t old = cur[ins->x]; cur[ins->x] = sp; radd(m, pcs, caps, cnt, pc + 1, cur, sp); cur[ins->x] = old; return; }
  case R_ASSERT: if (rassert(m, ins->x, sp)) radd(m, pcs, caps, cnt, pc + 1, cur, sp); return;
  default: {
    int nc = m->h->ncap;
    pcs[*cnt] = pc;
    memcpy(caps + (size_t)*cnt * (size_t)nc, cur, (size_t)nc * sizeof(int64_t));
    (*cnt)++;
  }
  }
}
/* leftmost-first search from byte offset `start`; fills out[0..ncap) with byte offsets (-1 unset) */
static int rvm_search(RVM* m, int64_t start, int64_t* out) {
  int nc = m->h->ncap, matched = 0;
  m->cn = 0;
  m->gen++;
  for (int64_t sp = start;;) {
    if (!matched) {
      for (int k = 0; k < nc; k++) m->tmp[k] = -1;
      radd(m, m->cpc, m->ccap, &m->cn, 0, m->tmp, sp);
    }
    if (m->cn == 0) {
      /* nothing alive here: try the next start position */
      if (matched || sp >= m->n) break;
      int l; utf8_at(m->s, m->n, sp, &l);
      sp += l;
      m->gen++;
      continue;
    }
    int l = 0; uint32_t c = 0;
    if (sp < m->n) c = utf8_at(m->s, m->n, sp, &l);
    m->gen++;
    m->nn = 0;
    for (int i = 0; i < m->cn; i++) {
      const RInst* ins = &m->in[m->cpc[i]];
      int64_t* tc = m->ccap + (size_t)i * (size_t)nc;
      int ok = 0;
      switch (ins->op) {
      case R_MATCH: memcpy(out, tc, (size_t)nc * sizeof(int64_t)); matched = 1; i = m->cn; continue;
      case R_CHAR: ok = sp < m->n && (ins->y ? fold_lower(c) : c) == (uint32_t)ins->x; break;
      case R_ANY: ok = sp < m->n && (ins->x || c != '\n'); break;
      case R_CLASS: ok = sp < m->n && rclass(m, ins, c); break;
      }
      if (ok) radd(m, m->npc, m->ncapv, &m->nn, m->cpc[i] + 1, tc, sp + l);
    }
    int32_t* tp = m->cpc; m->cpc = m->npc; m->npc = tp;
    int64_t* tcap = m->ccap; m->ccap = m->ncapv; m->ncapv = tcap;
    m->cn = m->nn;
    if (sp >= m->n) {
      /* threads that consumed nothing cannot exist past the end; run one last step for MATCH */
      if (m->cn == 0) break;
      for (int i = 0; i < m->cn; i++) if (m->in[m->cpc[i]].op == R_MATCH) { memcpy(out, m->ccap + (size_t)i * (size_t)nc, (size_t)nc * sizeof(int64_t)); matched = 1; break; }
      break;
    }
    sp += l;
  }
  return matched;
}
/* iterate matches the way find_all/split/replace_all see them (empty matches never touch the previous one) */
typedef struct RIter { RVM* m; int64_t pos, last_end; int64_t* caps; } RIter;
static int riter_next(RIter* it) {
  while (it->pos <= it->m->n) {
    if (!rvm_search(it->m, it->pos, it->caps)) return 0;
    int64_t s = it->caps[0], e = it->caps[1];
    if (s == e && s == it->last_end) {
      if (s >= it->m->n) return 0;
      int l; utf8_at(it->m->s, it->m->n, s, &l);
      it->pos = s + l;
      continue;
    }
    it->last_end = e;
    if (e > s) it->pos = e;
    else { if (e >= it->m->n) it->pos = e + 1; else { int l; utf8_at(it->m->s, it->m->n, e, &l); it->pos = e + l; } }
    return 1;
  }
  return 0;
}
static V make_match(RVM* m, const int64_t* caps, int64_t* cur_off, int64_t* cur_chars) {
  int ng = m->h->ncap / 2 - 1;
  *cur_chars += chars_before(m->s + *cur_off, caps[0] - *cur_off);
  *cur_off = caps[0];
  int64_t sc = *cur_chars;
  int64_t ec = sc + chars_before(m->s + caps[0], caps[1] - caps[0]);
  V groups = list_new(ng);
  for (int g = 1; g <= ng; g++) {
    int64_t a = caps[2 * g], b = caps[2 * g + 1];
    lpush(groups, a >= 0 && b >= a ? d_some(d_str_new(m->s + a, b - a)) : d_none());
  }
  V f[4] = {d_str_new(m->s + caps[0], caps[1] - caps[0]), d_int(sc), d_int(ec), groups};
  return d_adt(&d_td_re_Match, 0, 4, f);
}
static RVM* rvm_on(V rx, V text) { RVM* m = rvm_new(rx); m->s = S(text)->p; m->n = S(text)->len; return m; }
V d_n_re_Regex_pattern(V rx) { V p = d_retain(A(rx)->f[0]); d_release(rx); return p; }
V d_n_re_Regex_is_match(V rx, V text) {
  RVM* m = rvm_on(rx, text);
  int64_t* caps = xmalloc((size_t)m->h->ncap * sizeof(int64_t));
  int ok = rvm_search(m, 0, caps);
  free(caps); rvm_free(m); d_release(rx); d_release(text);
  return d_bool(ok);
}
V d_n_re_Regex_find(V rx, V text) {
  RVM* m = rvm_on(rx, text);
  int64_t* caps = xmalloc((size_t)m->h->ncap * sizeof(int64_t));
  V out = d_none();
  if (rvm_search(m, 0, caps)) { int64_t o = 0, c = 0; out = d_some(make_match(m, caps, &o, &c)); }
  free(caps); rvm_free(m); d_release(rx); d_release(text);
  return out;
}
V d_n_re_Regex_find_all(V rx, V text) {
  RVM* m = rvm_on(rx, text);
  RIter it = {m, 0, -1, xmalloc((size_t)m->h->ncap * sizeof(int64_t))};
  V out = list_new(0);
  int64_t o = 0, c = 0;
  while (riter_next(&it)) lpush(out, make_match(m, it.caps, &o, &c));
  free(it.caps); rvm_free(m); d_release(rx); d_release(text);
  return out;
}
V d_n_re_Regex_split(V rx, V text) {
  RVM* m = rvm_on(rx, text);
  RIter it = {m, 0, -1, xmalloc((size_t)m->h->ncap * sizeof(int64_t))};
  V out = list_new(0);
  int64_t from = 0;
  while (riter_next(&it)) { lpush(out, d_str_new(m->s + from, it.caps[0] - from)); from = it.caps[1]; }
  lpush(out, d_str_new(m->s + from, m->n - from));
  free(it.caps); rvm_free(m); d_release(rx); d_release(text);
  return out;
}
V d_n_re_Regex_replace_all(V rx, V text, V rep) {
  RVM* m = rvm_on(rx, text);
  int ng = m->h->ncap / 2 - 1;
  const char* rp = S(rep)->p; int64_t rl = S(rep)->len;
  /* validate group references once */
  for (int64_t i = 0; i < rl; i++) {
    if (rp[i] != '$') continue;
    if (i + 1 < rl && rp[i + 1] == '$') { i++; continue; }
    int g = -1;
    if (i + 1 < rl && rp[i + 1] >= '0' && rp[i + 1] <= '9') g = rp[i + 1] - '0';
    else if (i + 1 < rl && rp[i + 1] == '{') { char* e; long v = strtol(rp + i + 2, &e, 10); if (e != rp + i + 2 && *e == '}') g = (int)v; }
    if (g < 0) d_panic_c("re.replace_all: `$` must be followed by a group number (`$1`, `${12}`); write `$$` for a dollar sign");
    if (g > ng) panicf("re.replace_all: the replacement uses group $%d but the regex has %d group(s)", g, ng);
  }
  RIter it = {m, 0, -1, xmalloc((size_t)m->h->ncap * sizeof(int64_t))};
  DBuf b; d_buf_init(&b);
  int64_t from = 0;
  while (riter_next(&it)) {
    d_buf_add(&b, m->s + from, it.caps[0] - from);
    for (int64_t i = 0; i < rl; i++) {
      if (rp[i] != '$') { buf_ch(&b, rp[i]); continue; }
      if (rp[i + 1] == '$') { buf_ch(&b, '$'); i++; continue; }
      int g;
      if (rp[i + 1] == '{') { char* e; g = (int)strtol(rp + i + 2, &e, 10); i = e - rp; }
      else { g = rp[i + 1] - '0'; i++; }
      int64_t a = it.caps[2 * g], z = it.caps[2 * g + 1];
      if (a >= 0 && z >= a) d_buf_add(&b, m->s + a, z - a);
    }
    from = it.caps[1];
  }
  d_buf_add(&b, m->s + from, m->n - from);
  free(it.caps); rvm_free(m); d_release(rx); d_release(text); d_release(rep);
  return d_buf_done(&b);
}
V d_n_re_escape(V text) {
  DBuf b; d_buf_init(&b);
  for (int64_t i = 0; i < S(text)->len; i++) {
    char c = S(text)->p[i];
    if (strchr("\\.+*?()|[]{}^$#&-~", c) && c) buf_ch(&b, '\\');
    buf_ch(&b, c);
  }
  d_release(text);
  return d_buf_done(&b);
}

/* ------------------------------------------------------------------ html
   The tree lives flat in one Bytes value (header, nodes, attributes, string pool with the source),
   nodes in document order, so a Node is just (Document, index) and descendants are a contiguous range. */

enum { HK_DOC, HK_ELEM, HK_TEXT, HK_COMMENT };
#define HTML_MAX_DEPTH 512
typedef struct HNode {
  int32_t kind, parent, next, prev, first, last, end, attr0, nattr, name_len;
  int64_t name_off, text_off, text_len, src_lo, src_hi, in_lo, in_hi;
} HNode;
typedef struct HAttr { int64_t name_off, val_off; int32_t name_len, pad; int64_t val_len; } HAttr;
typedef struct HHead { int64_t nnodes, nattrs, pool_off, src_off, src_len; } HHead;

typedef struct HB { HNode* nd; int64_t nn, cn; HAttr* at; int64_t na, ca; DBuf pool; int32_t stack[HTML_MAX_DEPTH + 1]; int sp; const char* s; int64_t n; } HB;

static const struct { const char* n; uint32_t c; } html_ents[] = {
  {"amp", '&'}, {"lt", '<'}, {"gt", '>'}, {"quot", '"'}, {"apos", '\''}, {"nbsp", 0xA0}, {"copy", 0xA9}, {"reg", 0xAE},
  {"trade", 0x2122}, {"euro", 0x20AC}, {"cent", 0xA2}, {"pound", 0xA3}, {"yen", 0xA5}, {"sect", 0xA7}, {"deg", 0xB0},
  {"plusmn", 0xB1}, {"middot", 0xB7}, {"times", 0xD7}, {"divide", 0xF7}, {"laquo", 0xAB}, {"raquo", 0xBB},
  {"iexcl", 0xA1}, {"iquest", 0xBF}, {"ordf", 0xAA}, {"ordm", 0xBA}, {"sup2", 0xB2}, {"sup3", 0xB3}, {"frac12", 0xBD},
  {"ndash", 0x2013}, {"mdash", 0x2014}, {"lsquo", 0x2018}, {"rsquo", 0x2019}, {"ldquo", 0x201C}, {"rdquo", 0x201D},
  {"sbquo", 0x201A}, {"bdquo", 0x201E}, {"bull", 0x2022}, {"hellip", 0x2026}, {"prime", 0x2032}, {"larr", 0x2190},
  {"rarr", 0x2192}, {"uarr", 0x2191}, {"darr", 0x2193}, {"hearts", 0x2665}, {"check", 0x2713}, {"shy", 0xAD},
  {"Aacute", 0xC1}, {"Eacute", 0xC9}, {"Iacute", 0xCD}, {"Oacute", 0xD3}, {"Uacute", 0xDA}, {"Ntilde", 0xD1}, {"Uuml", 0xDC},
  {"aacute", 0xE1}, {"eacute", 0xE9}, {"iacute", 0xED}, {"oacute", 0xF3}, {"uacute", 0xFA}, {"ntilde", 0xF1}, {"uuml", 0xFC},
  {"Agrave", 0xC0}, {"Egrave", 0xC8}, {"agrave", 0xE0}, {"egrave", 0xE8}, {"igrave", 0xEC}, {"ograve", 0xF2}, {"ugrave", 0xF9},
  {"acirc", 0xE2}, {"ecirc", 0xEA}, {"ocirc", 0xF4}, {"atilde", 0xE3}, {"otilde", 0xF5}, {"auml", 0xE4}, {"euml", 0xEB},
  {"iuml", 0xEF}, {"ouml", 0xF6}, {"Auml", 0xC4}, {"Ouml", 0xD6}, {"ccedil", 0xE7}, {"Ccedil", 0xC7}, {"szlig", 0xDF},
  {"aring", 0xE5}, {"oslash", 0xF8}, {"aelig", 0xE6}, {"zwj", 0x200D}, {"zwnj", 0x200C}, {"thinsp", 0x2009},
  {"ensp", 0x2002}, {"emsp", 0x2003}, {"lrm", 0x200E}, {"rlm", 0x200F},
};
/* decode entities of s[0..n) into b */
static void html_decode(DBuf* b, const char* s, int64_t n) {
  for (int64_t i = 0; i < n;) {
    if (s[i] != '&') {
      const char* amp = memchr(s + i, '&', (size_t)(n - i));
      int64_t e = amp ? amp - s : n;
      d_buf_add(b, s + i, e - i);
      i = e;
      continue;
    }
    int64_t j = i + 1;
    if (j < n && s[j] == '#') {
      j++;
      int hex = j < n && (s[j] == 'x' || s[j] == 'X');
      if (hex) j++;
      int64_t st = j; uint32_t v = 0; int big = 0;
      while (j < n && (hex ? hexdig((unsigned char)s[j]) >= 0 : (s[j] >= '0' && s[j] <= '9'))) { v = v * (hex ? 16 : 10) + (uint32_t)(hex ? hexdig((unsigned char)s[j]) : s[j] - '0'); if (v > 0x10FFFF) big = 1; j++; }
      if (j > st) {
        if (j < n && s[j] == ';') j++;
        if (big || v == 0 || (v >= 0xD800 && v <= 0xDFFF)) v = 0xFFFD;
        utf8_put(b, v);
        i = j;
        continue;
      }
    } else {
      int64_t st = j;
      while (j < n && j - st < 32 && ((s[j] >= 'a' && s[j] <= 'z') || (s[j] >= 'A' && s[j] <= 'Z') || (s[j] >= '0' && s[j] <= '9'))) j++;
      int semi = j < n && s[j] == ';';
      for (size_t k = 0; k < sizeof html_ents / sizeof html_ents[0]; k++) {
        size_t l = strlen(html_ents[k].n);
        if ((int64_t)l == j - st && !memcmp(s + st, html_ents[k].n, l) && (semi || k < 8)) {
          utf8_put(b, html_ents[k].c);
          i = j + semi;
          goto next;
        }
      }
    }
    buf_ch(b, '&');
    i++;
  next:;
  }
}
static int64_t hpool(HB* h, const char* s, int64_t n) { int64_t o = h->pool.len; d_buf_add(&h->pool, s, n); return o; }
static int32_t hnode(HB* h, int kind) {
  if (h->nn == h->cn) { h->cn = h->cn ? h->cn * 2 : 64; h->nd = xrealloc(h->nd, (size_t)h->cn * sizeof(HNode)); }
  HNode* x = &h->nd[h->nn];
  memset(x, 0, sizeof *x);
  x->kind = kind; x->parent = x->next = x->prev = x->first = x->last = -1; x->end = (int32_t)h->nn;
  int32_t id = (int32_t)h->nn++;
  if (h->sp >= 0) {
    int32_t p = h->stack[h->sp];
    x = &h->nd[id];
    x->parent = p;
    if (h->nd[p].last >= 0) { h->nd[h->nd[p].last].next = id; x->prev = h->nd[p].last; } else h->nd[p].first = id;
    h->nd[p].last = id;
  }
  return id;
}
static int hname_is(HB* h, int32_t id, const char* name) {
  HNode* x = &h->nd[id];
  return x->kind == HK_ELEM && (size_t)x->name_len == strlen(name) && !memcmp(h->pool.p + x->name_off, name, (size_t)x->name_len);
}
static int name_in(const char* name, int64_t l, const char* list) {
  /* list: space-separated, with spaces at both ends */
  char key[40];
  if (l <= 0 || l > 30) return 0;
  key[0] = ' '; memcpy(key + 1, name, (size_t)l); key[l + 1] = ' '; key[l + 2] = 0;
  return strstr(list, key) != NULL;
}
static int hnode_in(HB* h, int32_t id, const char* list) { HNode* x = &h->nd[id]; return x->kind == HK_ELEM && name_in(h->pool.p + x->name_off, x->name_len, list); }
static void hpop(HB* h, int64_t at) {
  int32_t id = h->stack[h->sp--];
  HNode* x = &h->nd[id];
  x->end = (int32_t)h->nn - 1;
  if (!x->src_hi) { x->src_hi = at; x->in_hi = at; }
}
/* pop up to and including the nearest open element named in `targets`, unless a `stops` element comes first */
static void hclose_nearest(HB* h, const char* targets, const char* stops, int64_t at) {
  for (int k = h->sp; k > 0; k--) {
    int32_t id = h->stack[k];
    if (hnode_in(h, id, targets)) { while (h->sp >= k) hpop(h, at); return; }
    if (hnode_in(h, id, stops)) return;
  }
}
static const char* const H_VOID = " area base br col embed hr img input link meta param source track wbr ";
static const char* const H_PCLOSE = " address article aside blockquote details dialog div dl fieldset figcaption figure footer form h1 h2 h3 h4 h5 h6 header hgroup hr main menu nav ol p pre section table ul li dd dt ";
static const char* const H_SCOPE = " html body table td th caption button template object ";
static void himplied(HB* h, const char* t, int64_t l, int64_t at) {
  if (name_in(t, l, H_PCLOSE)) hclose_nearest(h, " p ", H_SCOPE, at);
  if (name_in(t, l, " li ")) hclose_nearest(h, " li ", " ul ol table td th html body ", at);
  else if (name_in(t, l, " dt dd ")) hclose_nearest(h, " dt dd ", " dl table td th html body ", at);
  else if (name_in(t, l, " option ")) { if (h->sp > 0 && hnode_in(h, h->stack[h->sp], " option ")) hpop(h, at); }
  else if (name_in(t, l, " optgroup ")) hclose_nearest(h, " option optgroup ", " select ", at);
  else if (name_in(t, l, " tr ")) hclose_nearest(h, " tr ", " table thead tbody tfoot ", at);
  else if (name_in(t, l, " td th ")) hclose_nearest(h, " td th ", " tr table ", at);
  else if (name_in(t, l, " thead tbody tfoot ")) hclose_nearest(h, " thead tbody tfoot ", " table ", at);
  else if (name_in(t, l, " h1 h2 h3 h4 h5 h6 ")) { if (h->sp > 0 && hnode_in(h, h->stack[h->sp], " h1 h2 h3 h4 h5 h6 ")) hpop(h, at); }
  else if (name_in(t, l, " a ")) hclose_nearest(h, " a ", " table td th button ", at);
}
static void htext(HB* h, const char* s, int64_t n, int decode, int64_t lo) {
  if (n <= 0) return;
  int32_t p = h->stack[h->sp];
  int32_t last = h->nd[p].last;
  int64_t off = h->pool.len;
  if (decode) html_decode(&h->pool, s, n); else d_buf_add(&h->pool, s, n);
  int64_t len = h->pool.len - off;
  if (last >= 0 && h->nd[last].kind == HK_TEXT && h->nd[last].text_off + h->nd[last].text_len == off) {
    h->nd[last].text_len += len; h->nd[last].src_hi = lo + n;
    return;
  }
  int32_t id = hnode(h, HK_TEXT);
  h->nd[id].text_off = off; h->nd[id].text_len = len; h->nd[id].src_lo = lo; h->nd[id].src_hi = lo + n;
}
static int hname_char(char c) { return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '-' || c == '_' || c == ':' || c == '.'; }
static int hws(char c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f'; }
static int64_t find_ci(const char* s, int64_t n, int64_t from, const char* pat) {
  int64_t l = (int64_t)strlen(pat);
  for (int64_t i = from; i + l <= n; i++) if (!strncasecmp(s + i, pat, (size_t)l)) return i;
  return -1;
}
static void hparse(HB* h) {
  const char* s = h->s; int64_t n = h->n, i = 0;
  while (i < n) {
    if (s[i] != '<') {
      const char* lt = memchr(s + i, '<', (size_t)(n - i));
      int64_t e = lt ? lt - s : n;
      htext(h, s + i, e - i, 1, i);
      i = e;
      continue;
    }
    if (i + 3 < n && !strncmp(s + i, "<!--", 4)) {
      int64_t e = find_ci(s, n, i + 4, "-->");
      int64_t ce = e < 0 ? n : e;
      int32_t id = hnode(h, HK_COMMENT);
      h->nd[id].text_off = hpool(h, s + i + 4, ce - i - 4); h->nd[id].text_len = ce - i - 4;
      h->nd[id].src_lo = i; i = e < 0 ? n : e + 3; h->nd[id].src_hi = i;
      continue;
    }
    if (i + 1 < n && (s[i + 1] == '!' || s[i + 1] == '?')) {
      const char* gt = memchr(s + i, '>', (size_t)(n - i));
      i = gt ? gt - s + 1 : n;
      continue;
    }
    if (i + 2 < n && s[i + 1] == '/' && ((s[i + 2] | 32) >= 'a' && (s[i + 2] | 32) <= 'z')) {
      int64_t ns = i + 2, ne = ns;
      while (ne < n && hname_char(s[ne])) ne++;
      char name[64]; int64_t nl = ne - ns < 63 ? ne - ns : 63;
      for (int64_t k = 0; k < nl; k++) name[k] = (char)((s[ns + k] >= 'A' && s[ns + k] <= 'Z') ? s[ns + k] + 32 : s[ns + k]);
      name[nl] = 0;
      const char* gt = memchr(s + ne, '>', (size_t)(n - ne));
      int64_t after = gt ? gt - s + 1 : n;
      for (int k = h->sp; k > 0; k--) {
        int32_t id = h->stack[k];
        if (hname_is(h, id, name)) {
          while (h->sp > k) hpop(h, i);
          h->nd[id].in_hi = i; h->nd[id].src_hi = after;
          hpop(h, after);
          break;
        }
        if (name_in(name, nl, " p li dd dt ") ? hnode_in(h, id, H_SCOPE) : 0) break;
      }
      i = after;
      continue;
    }
    if (i + 1 < n && ((s[i + 1] | 32) >= 'a' && (s[i + 1] | 32) <= 'z')) {
      int64_t lo = i, ns = i + 1, ne = ns;
      while (ne < n && hname_char(s[ne])) ne++;
      char name[64]; int64_t nl = ne - ns < 63 ? ne - ns : 63;
      for (int64_t k = 0; k < nl; k++) name[k] = (char)((s[ns + k] >= 'A' && s[ns + k] <= 'Z') ? s[ns + k] + 32 : s[ns + k]);
      name[nl] = 0;
      himplied(h, name, nl, lo);
      int32_t id = hnode(h, HK_ELEM);
      h->nd[id].name_off = hpool(h, name, nl); h->nd[id].name_len = (int32_t)nl; h->nd[id].src_lo = lo;
      h->nd[id].attr0 = (int32_t)h->na;
      int64_t j = ne; int selfclose = 0;
      for (;;) {
        while (j < n && (hws(s[j]) || s[j] == '/')) { if (s[j] == '/' && j + 1 < n && s[j + 1] == '>') selfclose = 1; j++; }
        if (j >= n || s[j] == '>') { if (j < n) j++; break; }
        int64_t as = j;
        while (j < n && !hws(s[j]) && s[j] != '=' && s[j] != '>' && !(s[j] == '/' && j + 1 < n && s[j + 1] == '>')) j++;
        if (j == as) { j++; continue; }
        int64_t ae = j;
        while (j < n && hws(s[j])) j++;
        int64_t vs = 0, ve = 0; int has_v = 0;
        if (j < n && s[j] == '=') {
          j++;
          while (j < n && hws(s[j])) j++;
          if (j < n && (s[j] == '"' || s[j] == '\'')) {
            char q = s[j++]; vs = j;
            const char* qe = memchr(s + j, q, (size_t)(n - j));
            ve = qe ? qe - s : n; j = qe ? ve + 1 : n;
          } else { vs = j; while (j < n && !hws(s[j]) && s[j] != '>') j++; ve = j; }
          has_v = 1;
        }
        /* duplicate attributes: the first one wins */
        int dup = 0;
        for (int64_t k = h->nd[id].attr0; k < h->na && !dup; k++)
          if (h->at[k].name_len == ae - as && !strncasecmp(h->pool.p + h->at[k].name_off, s + as, (size_t)(ae - as))) dup = 1;
        if (dup) continue;
        if (h->na == h->ca) { h->ca = h->ca ? h->ca * 2 : 64; h->at = xrealloc(h->at, (size_t)h->ca * sizeof(HAttr)); }
        HAttr* a = &h->at[h->na++];
        memset(a, 0, sizeof *a);
        a->name_off = hpool(h, s + as, ae - as); a->name_len = (int32_t)(ae - as);
        lower_ascii(h->pool.p + a->name_off, ae - as);
        a->val_off = h->pool.len;
        if (has_v) html_decode(&h->pool, s + vs, ve - vs);
        a->val_len = h->pool.len - a->val_off;
      }
      h->nd[id].nattr = (int32_t)(h->na - h->nd[id].attr0);
      h->nd[id].in_lo = j;
      if (name_in(name, nl, H_VOID) || (selfclose && !name_in(name, nl, " script style textarea title div span p a li ul ol table td tr "))) {
        h->nd[id].src_hi = h->nd[id].in_hi = j;
        i = j;
        continue;
      }
      if (name_in(name, nl, " script style textarea title xmp ")) {
        char close[72]; snprintf(close, sizeof close, "</%s", name);
        int64_t e = find_ci(s, n, j, close);
        int64_t ce = e < 0 ? n : e;
        if (h->sp < HTML_MAX_DEPTH) {
          h->stack[++h->sp] = id;
          htext(h, s + j, ce - j, !name_in(name, nl, " script style xmp "), j);
          h->nd[id].in_hi = ce;
          const char* gt = e < 0 ? NULL : memchr(s + ce, '>', (size_t)(n - ce));
          i = gt ? gt - s + 1 : n;
          h->nd[id].src_hi = i;
          hpop(h, i);
        } else i = ce;
        continue;
      }
      if (h->sp < HTML_MAX_DEPTH) h->stack[++h->sp] = id;
      i = j;
      continue;
    }
    htext(h, s + i, 1, 0, i);
    i++;
  }
  while (h->sp > 0) hpop(h, n);
  h->nd[0].end = (int32_t)h->nn - 1;
}
V d_n_html_parse(V src) {
  HB h; memset(&h, 0, sizeof h);
  h.s = S(src)->p; h.n = S(src)->len;
  d_buf_init(&h.pool);
  h.sp = -1;
  int32_t root = hnode(&h, HK_DOC);
  h.stack[0] = root; h.sp = 0;
  h.nd[root].src_hi = h.nd[root].in_hi = h.n;
  int64_t src_off = hpool(&h, h.s, h.n);
  hparse(&h);
  HHead hd = {h.nn, h.na, 0, src_off, h.n};
  int64_t sz = (int64_t)sizeof hd + h.nn * (int64_t)sizeof(HNode) + h.na * (int64_t)sizeof(HAttr);
  hd.pool_off = sz;
  V blob = str_alloc(K_BYTES, sz + h.pool.len);
  char* q = S(blob)->p;
  memcpy(q, &hd, sizeof hd);
  memcpy(q + sizeof hd, h.nd, (size_t)h.nn * sizeof(HNode));
  if (h.na) memcpy(q + sizeof hd + (size_t)h.nn * sizeof(HNode), h.at, (size_t)h.na * sizeof(HAttr));
  memcpy(q + sz, h.pool.p, (size_t)h.pool.len);
  free(h.nd); free(h.at); free(h.pool.p);
  d_release(src);
  return d_adt(&d_td_html_Document, 0, 1, &blob);
}

/* ---- reading the flat tree */
typedef struct HT { const HHead* h; const HNode* nd; const HAttr* at; const char* pool; } HT;
static HT html_tree(V doc) {
  const char* p = S(A(doc)->f[0])->p;
  HT t;
  t.h = (const HHead*)p;
  t.nd = (const HNode*)(p + sizeof(HHead));
  t.at = (const HAttr*)(p + sizeof(HHead) + (size_t)t.h->nnodes * sizeof(HNode));
  t.pool = p + t.h->pool_off;
  return t;
}
static int ht_name(const HT* t, int32_t id, const char* name, int64_t l) {
  const HNode* x = &t->nd[id];
  return x->kind == HK_ELEM && x->name_len == l && !memcmp(t->pool + x->name_off, name, (size_t)l);
}
static const HAttr* ht_attr(const HT* t, int32_t id, const char* name, int64_t l) {
  const HNode* x = &t->nd[id];
  for (int k = 0; k < x->nattr; k++) {
    const HAttr* a = &t->at[x->attr0 + k];
    if (a->name_len == l && !strncasecmp(t->pool + a->name_off, name, (size_t)l)) return a;
  }
  return NULL;
}
static V node_new(V doc, int32_t id) { V f[2] = {d_retain(doc), d_int(id)}; return d_adt(&d_td_html_Node, 0, 2, f); }
static void html_show(DBuf* b, V v) {
  if (A(v)->t == &d_td_html_Document) { HT t = html_tree(v); buf_fmt(b, "Document(%lld nodes)", (long long)t.h->nnodes); return; }
  HT t = html_tree(A(v)->f[0]);
  const HNode* x = &t.nd[A(v)->f[1].i];
  buf_cstr(b, "Node(<");
  d_buf_add(b, t.pool + x->name_off, x->name_len);
  for (int k = 0; k < x->nattr && k < 3; k++) {
    const HAttr* a = &t.at[x->attr0 + k];
    buf_ch(b, ' '); d_buf_add(b, t.pool + a->name_off, a->name_len);
    buf_cstr(b, "="); show_quoted(b, t.pool + a->val_off, a->val_len > 60 ? 60 : a->val_len, 0);
  }
  buf_cstr(b, x->nattr > 3 ? " ...>)" : ">)");
}
static const char* const H_BLOCK = " address article aside blockquote br dd details div dl dt fieldset figcaption figure footer form h1 h2 h3 h4 h5 h6 header hr li main nav ol option p pre section summary table tbody td tfoot th thead title tr ul ";
static void ht_text(const HT* t, int32_t id, DBuf* b, int depth) {
  for (int32_t c = t->nd[id].first; c >= 0; c = t->nd[c].next) {
    const HNode* x = &t->nd[c];
    if (x->kind == HK_TEXT) d_buf_add(b, t->pool + x->text_off, x->text_len);
    else if (x->kind == HK_ELEM && depth < HTML_MAX_DEPTH + 2) {
      const char* nm = t->pool + x->name_off;
      if (name_in(nm, x->name_len, " script style template noscript head ")) continue;
      int blk = name_in(nm, x->name_len, H_BLOCK);
      if (blk) buf_ch(b, ' ');
      ht_text(t, c, b, depth + 1);
      if (blk) buf_ch(b, ' ');
    }
  }
}
static V collapse_ws(DBuf* b) {
  DBuf o; d_buf_init(&o);
  int pend = 0;
  for (int64_t i = 0; i < b->len;) {
    int l; uint32_t c = utf8_at(b->p, b->len, i, &l);
    if (is_ws(c)) { pend = o.len > 0; i += l; continue; }
    if (pend) { buf_ch(&o, ' '); pend = 0; }
    d_buf_add(&o, b->p + i, l);
    i += l;
  }
  free(b->p);
  return d_buf_done(&o);
}
static V node_text(V doc, int32_t id) {
  HT t = html_tree(doc);
  DBuf b; d_buf_init(&b);
  const HNode* x = &t.nd[id];
  if (x->kind == HK_ELEM && name_in(t.pool + x->name_off, x->name_len, " script style title textarea ")) {
    for (int32_t c = x->first; c >= 0; c = t.nd[c].next) if (t.nd[c].kind == HK_TEXT) d_buf_add(&b, t.pool + t.nd[c].text_off, t.nd[c].text_len);
  } else ht_text(&t, id, &b, 0);
  return collapse_ws(&b);
}

/* ---- CSS selectors */
enum { SS_TAG, SS_ID, SS_CLASS, SS_ATTR, SS_FIRST, SS_LAST, SS_ONLY, SS_NTH, SS_NTHL, SS_EMPTY, SS_NOT };
typedef struct SSim { int k; const char* name; int64_t nl; const char* val; int64_t vl; char op; int64_t a, b; int not_at; } SSim;
#define SEL_MAX 64
typedef struct SComp { SSim s[16]; int n; } SComp;
typedef struct SCx { int first, n; char comb[16]; } SCx;  /* compounds first..first+n-1 */
typedef struct Sel {
  SComp comps[SEL_MAX]; int nc;
  SCx cx[32]; int nx;
  char* buf; int64_t len, i;
  const char* src;
} Sel;
static _Noreturn void sel_fail(Sel* s, const char* msg) {
  panicf("invalid CSS selector `%.200s`: %s", s->src, msg);
}
static void sel_ws(Sel* s) { while (s->i < s->len && hws(s->buf[s->i])) s->i++; }
static int sel_ident_char(char c) { return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '-' || c == '_' || (unsigned char)c >= 0x80; }
/* identifier in place: returns start, sets length */
static const char* sel_ident(Sel* s, int64_t* l) {
  int64_t st = s->i;
  while (s->i < s->len && sel_ident_char(s->buf[s->i])) s->i++;
  *l = s->i - st;
  if (!*l) sel_fail(s, "expected a name");
  return s->buf + st;
}
static void sel_nth(Sel* s, SSim* x) {
  /* an+b | odd | even | n */
  sel_ws(s);
  int64_t st = s->i;
  while (s->i < s->len && s->buf[s->i] != ')') s->i++;
  if (s->i >= s->len) sel_fail(s, "missing `)`");
  char t[64]; int64_t tl = 0;
  for (int64_t k = st; k < s->i && tl < 63; k++) if (!hws(s->buf[k])) t[tl++] = (char)((s->buf[k] >= 'A' && s->buf[k] <= 'Z') ? s->buf[k] + 32 : s->buf[k]);
  t[tl] = 0;
  s->i++;
  if (!strcmp(t, "odd")) { x->a = 2; x->b = 1; return; }
  if (!strcmp(t, "even")) { x->a = 2; x->b = 0; return; }
  char* np = strchr(t, 'n');
  char* e;
  if (!np) { x->a = 0; x->b = strtol(t, &e, 10); if (e == t || *e) sel_fail(s, "bad :nth-child argument"); return; }
  *np = 0;
  if (!t[0] || !strcmp(t, "+")) x->a = 1; else if (!strcmp(t, "-")) x->a = -1; else { x->a = strtol(t, &e, 10); if (*e) sel_fail(s, "bad :nth-child argument"); }
  x->b = 0;
  if (np[1]) { x->b = strtol(np + 1, &e, 10); if (*e || (np[1] != '+' && np[1] != '-')) sel_fail(s, "bad :nth-child argument"); }
}
static int sel_compound(Sel* s, int depth);
static void sel_simple_list(Sel* s, SComp* c, int depth) {
  int any = 0;
  for (;;) {
    if (s->i >= s->len) break;
    char ch = s->buf[s->i];
    if (c->n >= 16) sel_fail(s, "too many parts in one compound selector");
    SSim* x = &c->s[c->n];
    memset(x, 0, sizeof *x);
    if (ch == '*' && !any) { s->i++; any = 1; continue; }
    if (sel_ident_char(ch) && !any && ch != '.' && ch != ':') {
      x->k = SS_TAG; x->name = sel_ident(s, &x->nl);
      for (int64_t k = 0; k < x->nl; k++) if (s->buf[x->name - s->buf + k] >= 'A' && s->buf[x->name - s->buf + k] <= 'Z') s->buf[x->name - s->buf + k] += 32;
      c->n++; any = 1; continue;
    }
    any = 1;
    if (ch == '#') { s->i++; x->k = SS_ID; x->val = sel_ident(s, &x->vl); c->n++; continue; }
    if (ch == '.') { s->i++; x->k = SS_CLASS; x->val = sel_ident(s, &x->vl); c->n++; continue; }
    if (ch == '[') {
      s->i++; sel_ws(s);
      x->k = SS_ATTR; x->name = sel_ident(s, &x->nl);
      sel_ws(s);
      if (s->i < s->len && s->buf[s->i] != ']') {
        char o = s->buf[s->i];
        if (o == '=') { x->op = '='; s->i++; }
        else if (strchr("~^$*|", o) && s->i + 1 < s->len && s->buf[s->i + 1] == '=') { x->op = o; s->i += 2; }
        else sel_fail(s, "bad attribute operator (use = ~= ^= $= *= |=)");
        sel_ws(s);
        if (s->i < s->len && (s->buf[s->i] == '"' || s->buf[s->i] == '\'')) {
          char q = s->buf[s->i++];
          int64_t st = s->i, w = s->i;
          while (s->i < s->len && s->buf[s->i] != q) {
            if (s->buf[s->i] == '\\' && s->i + 1 < s->len) s->i++;
            s->buf[w++] = s->buf[s->i++];
          }
          if (s->i >= s->len) sel_fail(s, "unterminated string");
          s->i++;
          x->val = s->buf + st; x->vl = w - st;
        } else x->val = sel_ident(s, &x->vl);
        sel_ws(s);
      }
      if (s->i >= s->len || s->buf[s->i] != ']') sel_fail(s, "missing `]`");
      s->i++;
      c->n++;
      continue;
    }
    if (ch == ':') {
      s->i++;
      if (s->i < s->len && s->buf[s->i] == ':') sel_fail(s, "pseudo-elements (::) select nothing in a parsed document");
      int64_t l; const char* nm = sel_ident(s, &l);
#define IS(w) (l == (int64_t)strlen(w) && !strncasecmp(nm, w, (size_t)l))
      if (IS("first-child")) x->k = SS_FIRST;
      else if (IS("last-child")) x->k = SS_LAST;
      else if (IS("only-child")) x->k = SS_ONLY;
      else if (IS("empty")) x->k = SS_EMPTY;
      else if (IS("nth-child") || IS("nth-last-child")) {
        x->k = IS("nth-child") ? SS_NTH : SS_NTHL;
        if (s->i >= s->len || s->buf[s->i] != '(') sel_fail(s, "expected `(` after :nth-child");
        s->i++;
        sel_nth(s, x);
      } else if (IS("not")) {
        if (s->i >= s->len || s->buf[s->i] != '(') sel_fail(s, "expected `(` after :not");
        if (depth > 3) sel_fail(s, ":not nested too deeply");
        s->i++; sel_ws(s);
        x->k = SS_NOT;
        c->n++;
        x->not_at = sel_compound(s, depth + 1);
        sel_ws(s);
        if (s->i >= s->len || s->buf[s->i] != ')') sel_fail(s, ":not takes one compound selector such as `:not(.ad)`");
        s->i++;
        continue;
      } else sel_fail(s, "unsupported pseudo-class (supported: first-child last-child only-child nth-child nth-last-child empty not)");
#undef IS
      c->n++;
      continue;
    }
    break;
  }
  if (!any) sel_fail(s, "expected a selector");
}
static int sel_compound(Sel* s, int depth) {
  if (s->nc >= SEL_MAX) sel_fail(s, "selector too long");
  int id = s->nc++;
  s->comps[id].n = 0;
  sel_simple_list(s, &s->comps[id], depth);
  return id;
}
static void sel_parse(Sel* s) {
  for (;;) {
    sel_ws(s);
    if (s->nx >= 32) sel_fail(s, "too many selectors in the group");
    SCx* x = &s->cx[s->nx++];
    x->n = 0;
    char comb = 0;
    for (;;) {
      if (x->n >= 16) sel_fail(s, "too many combinators");
      x->comb[x->n] = comb;
      int c = sel_compound(s, 0);
      if (x->n == 0) x->first = c;
      x->n++;
      int had_ws = s->i < s->len && hws(s->buf[s->i]);
      sel_ws(s);
      if (s->i >= s->len || s->buf[s->i] == ',') break;
      char ch = s->buf[s->i];
      if (ch == '>' || ch == '+' || ch == '~') { comb = ch; s->i++; sel_ws(s); }
      else if (had_ws) comb = ' ';
      else sel_fail(s, "unexpected character");
      if (s->i >= s->len) sel_fail(s, "a combinator needs a selector after it");
    }
    if (s->i >= s->len) break;
    s->i++; /* , */
  }
}
static int32_t prev_elem(const HT* t, int32_t id) { for (int32_t p = t->nd[id].prev; p >= 0; p = t->nd[p].prev) if (t->nd[p].kind == HK_ELEM) return p; return -1; }
static int32_t next_elem(const HT* t, int32_t id) { for (int32_t p = t->nd[id].next; p >= 0; p = t->nd[p].next) if (t->nd[p].kind == HK_ELEM) return p; return -1; }
static int nth_ok(int64_t a, int64_t b, int64_t pos) {
  if (a == 0) return pos == b;
  int64_t d = pos - b;
  return d % a == 0 && d / a >= 0;
}
static int word_in(const char* list, int64_t ll, const char* w, int64_t wl) {
  for (int64_t i = 0; i < ll;) {
    while (i < ll && hws(list[i])) i++;
    int64_t st = i;
    while (i < ll && !hws(list[i])) i++;
    if (i - st == wl && wl && !memcmp(list + st, w, (size_t)wl)) return 1;
  }
  return 0;
}
static int match_comp(const Sel* s, const HT* t, int ci, int32_t id) {
  const SComp* c = &s->comps[ci];
  const HNode* x = &t->nd[id];
  if (x->kind != HK_ELEM) return 0;
  for (int k = 0; k < c->n; k++) {
    const SSim* q = &c->s[k];
    switch (q->k) {
    case SS_TAG: if (!ht_name(t, id, q->name, q->nl)) return 0; break;
    case SS_ID: { const HAttr* a = ht_attr(t, id, "id", 2); if (!a || a->val_len != q->vl || memcmp(t->pool + a->val_off, q->val, (size_t)q->vl)) return 0; break; }
    case SS_CLASS: { const HAttr* a = ht_attr(t, id, "class", 5); if (!a || !word_in(t->pool + a->val_off, a->val_len, q->val, q->vl)) return 0; break; }
    case SS_ATTR: {
      const HAttr* a = ht_attr(t, id, q->name, q->nl);
      if (!a) return 0;
      if (!q->op) break;
      const char* v = t->pool + a->val_off; int64_t vl = a->val_len;
      int ok;
      switch (q->op) {
      case '=': ok = vl == q->vl && !memcmp(v, q->val, (size_t)vl); break;
      case '~': ok = word_in(v, vl, q->val, q->vl); break;
      case '^': ok = q->vl && vl >= q->vl && !memcmp(v, q->val, (size_t)q->vl); break;
      case '$': ok = q->vl && vl >= q->vl && !memcmp(v + vl - q->vl, q->val, (size_t)q->vl); break;
      case '*': ok = q->vl && memmem(v, (size_t)vl, q->val, (size_t)q->vl) != NULL; break;
      default: ok = (vl == q->vl && !memcmp(v, q->val, (size_t)vl)) || (vl > q->vl && !memcmp(v, q->val, (size_t)q->vl) && v[q->vl] == '-');
      }
      if (!ok) return 0;
      break;
    }
    case SS_FIRST: if (prev_elem(t, id) >= 0) return 0; break;
    case SS_LAST: if (next_elem(t, id) >= 0) return 0; break;
    case SS_ONLY: if (prev_elem(t, id) >= 0 || next_elem(t, id) >= 0) return 0; break;
    case SS_EMPTY: for (int32_t ch = x->first; ch >= 0; ch = t->nd[ch].next) if (t->nd[ch].kind == HK_ELEM || (t->nd[ch].kind == HK_TEXT && t->nd[ch].text_len)) return 0; break;
    case SS_NTH: case SS_NTHL: {
      int64_t pos = 1;
      for (int32_t p = q->k == SS_NTH ? prev_elem(t, id) : next_elem(t, id); p >= 0; p = q->k == SS_NTH ? prev_elem(t, p) : next_elem(t, p)) pos++;
      if (!nth_ok(q->a, q->b, pos)) return 0;
      break;
    }
    case SS_NOT: if (match_comp(s, t, q->not_at, id)) return 0; break;
    }
  }
  return 1;
}
static int32_t parent_elem(const HT* t, int32_t id) { int32_t p = t->nd[id].parent; return p >= 0 && t->nd[p].kind == HK_ELEM ? p : -1; }
static int match_cx(const Sel* s, const HT* t, const SCx* x, int k, int32_t id) {
  if (!match_comp(s, t, x->first + k, id)) return 0;
  if (k == 0) return 1;
  switch (x->comb[k]) {
  case '>': { int32_t p = parent_elem(t, id); return p >= 0 && match_cx(s, t, x, k - 1, p); }
  case '+': { int32_t p = prev_elem(t, id); return p >= 0 && match_cx(s, t, x, k - 1, p); }
  case '~': for (int32_t p = prev_elem(t, id); p >= 0; p = prev_elem(t, p)) if (match_cx(s, t, x, k - 1, p)) return 1; return 0;
  default: for (int32_t p = parent_elem(t, id); p >= 0; p = parent_elem(t, p)) if (match_cx(s, t, x, k - 1, p)) return 1; return 0;
  }
}
/* elements in (scope, end] matching css, in document order; first_only stops at one */
static V html_select(V doc, int32_t scope, V css, int first_only) {
  Sel* s = xmalloc(sizeof(Sel));
  s->nc = s->nx = 0;
  s->len = S(css)->len; s->i = 0;
  s->buf = xmalloc((size_t)s->len + 1); memcpy(s->buf, S(css)->p, (size_t)s->len + 1);
  s->src = S(css)->p;
  sel_parse(s);
  HT t = html_tree(doc);
  V out = first_only ? d_none() : list_new(0);
  int32_t end = t.nd[scope].end;
  for (int32_t id = scope + 1; id <= end; id++) {
    if (t.nd[id].kind != HK_ELEM) continue;
    int hit = 0;
    for (int g = 0; g < s->nx && !hit; g++) hit = match_cx(s, &t, &s->cx[g], s->cx[g].n - 1, id);
    if (!hit) continue;
    if (first_only) { out = d_some(node_new(doc, id)); break; }
    lpush(out, node_new(doc, id));
  }
  free(s->buf); free(s);
  return out;
}
V d_n_html_Document_select(V doc, V css) { V r = html_select(doc, 0, css, 0); d_release(doc); d_release(css); return r; }
V d_n_html_Document_select_first(V doc, V css) { V r = html_select(doc, 0, css, 1); d_release(doc); d_release(css); return r; }
V d_n_html_Document_text(V doc) { V r = node_text(doc, 0); d_release(doc); return r; }
#define NODE_DOC(v) (A(v)->f[0])
#define NODE_ID(v) ((int32_t)A(v)->f[1].i)
V d_n_html_Node_select(V n, V css) { V r = html_select(NODE_DOC(n), NODE_ID(n), css, 0); d_release(n); d_release(css); return r; }
V d_n_html_Node_select_first(V n, V css) { V r = html_select(NODE_DOC(n), NODE_ID(n), css, 1); d_release(n); d_release(css); return r; }
V d_n_html_Node_text(V n) { V r = node_text(NODE_DOC(n), NODE_ID(n)); d_release(n); return r; }
V d_n_html_Node_tag(V n) {
  HT t = html_tree(NODE_DOC(n)); const HNode* x = &t.nd[NODE_ID(n)];
  V r = d_str_new(t.pool + x->name_off, x->name_len);
  d_release(n);
  return r;
}
V d_n_html_Node_attr(V n, V name) {
  HT t = html_tree(NODE_DOC(n));
  const HAttr* a = ht_attr(&t, NODE_ID(n), S(name)->p, S(name)->len);
  V r = a ? d_some(d_str_new(t.pool + a->val_off, a->val_len)) : d_none();
  d_release(n); d_release(name);
  return r;
}
V d_n_html_Node_attrs(V n) {
  HT t = html_tree(NODE_DOC(n)); const HNode* x = &t.nd[NODE_ID(n)];
  V m = map_new(K_MAP, x->nattr);
  for (int k = 0; k < x->nattr; k++) {
    const HAttr* a = &t.at[x->attr0 + k];
    map_put(M(m), d_str_new(t.pool + a->name_off, a->name_len), d_str_new(t.pool + a->val_off, a->val_len));
  }
  d_release(n);
  return m;
}
static V node_src(V n, int inner) {
  HT t = html_tree(NODE_DOC(n)); const HNode* x = &t.nd[NODE_ID(n)];
  int64_t lo = inner ? x->in_lo : x->src_lo, hi = inner ? x->in_hi : x->src_hi;
  if (hi < lo) hi = lo;
  const char* src = t.pool + t.h->src_off;
  /* keep UTF-8 boundaries (positions always sit on ASCII markup, but be safe) */
  V r = d_str_new(src + lo, hi - lo);
  d_release(n);
  return r;
}
V d_n_html_Node_html(V n) { return node_src(n, 0); }
V d_n_html_Node_inner_html(V n) { return node_src(n, 1); }
V d_n_html_Node_children(V n) {
  HT t = html_tree(NODE_DOC(n));
  V out = list_new(0);
  for (int32_t c = t.nd[NODE_ID(n)].first; c >= 0; c = t.nd[c].next) if (t.nd[c].kind == HK_ELEM) lpush(out, node_new(NODE_DOC(n), c));
  d_release(n);
  return out;
}
V d_n_html_Node_parent(V n) {
  HT t = html_tree(NODE_DOC(n));
  int32_t p = parent_elem(&t, NODE_ID(n));
  V r = p >= 0 ? d_some(node_new(NODE_DOC(n), p)) : d_none();
  d_release(n);
  return r;
}

/* ------------------------------------------------------------------ sandbox
   `main` declares what the program may do; the kernel enforces it.
   - no `fs` (and no `db`): Landlock denies every filesystem access. With `net`, the files TLS and DNS need
     stay readable (certificates, resolver config, system libraries); OpenSSL is loaded beforehand.
   - seccomp: no `net` (and no `db`) => socket() fails with EACCES; no `proc` => exec, fork and non-thread
     clone fail with EACCES; always: ptrace, bpf, io_uring, mount, namespaces, keyrings, modules.
   DRIFT_SANDBOX=off disables it, =strict panics if a layer is unavailable; DRIFT_SANDBOX_REPORT=1 prints
   the active layers and probes to stderr. Best effort by default: old kernels run unrestricted. */

#if defined(__SANITIZE_ADDRESS__) || defined(__SANITIZE_THREAD__)
#define D_SANITIZED 1   /* LeakSanitizer needs ptrace, a non-thread clone and /proc */
#else
#define D_SANITIZED 0
#endif
enum { EF_IO = 1, EF_FS = 2, EF_NET = 4, EF_DB = 8, EF_ENV = 16, EF_TIME = 32, EF_RAND = 64, EF_PROC = 128, EF_SPAWN = 256 };

#ifndef SYS_landlock_create_ruleset
#define SYS_landlock_create_ruleset 444
#define SYS_landlock_add_rule 445
#define SYS_landlock_restrict_self 446
#endif
struct d_ll_ruleset { uint64_t handled_access_fs; };
struct d_ll_path { uint64_t allowed_access; int32_t parent_fd; } __attribute__((packed));
#define LL_READ_FILE (1ull << 2)
#define LL_READ_DIR (1ull << 3)

static int ll_abi;
/* 1 applied, 0 unsupported by the kernel, -1 failed */
static int sb_landlock(int net) {
  long abi = syscall(SYS_landlock_create_ruleset, NULL, 0, 1u /* LANDLOCK_CREATE_RULESET_VERSION */);
  if (abi < 1) return 0;
  ll_abi = (int)abi;
  uint64_t handled = (1ull << 13) - 1;
  if (abi >= 2) handled |= 1ull << 13;  /* REFER */
  if (abi >= 3) handled |= 1ull << 14;  /* TRUNCATE */
  if (abi >= 5) handled |= 1ull << 15;  /* IOCTL_DEV */
  struct d_ll_ruleset attr = {handled};
  int rs = (int)syscall(SYS_landlock_create_ruleset, &attr, sizeof attr, 0);
  if (rs < 0) return -1;
  static const char* const net_paths[] = {
    "/etc/resolv.conf", "/etc/hosts", "/etc/nsswitch.conf", "/etc/gai.conf", "/etc/host.conf", "/etc/services",
    "/etc/protocols", "/etc/ssl", "/etc/pki", "/etc/ca-certificates", "/usr/lib/ssl", "/usr/share/ca-certificates",
    "/usr/local/share/ca-certificates", "/run/systemd/resolve", "/lib", "/lib64", "/usr/lib", "/usr/lib64", NULL};
  static const char* const san_paths[] = {"/proc", "/sys/devices/system/cpu", NULL};
  const char* const* lists[2] = {net ? net_paths : NULL, D_SANITIZED ? san_paths : NULL};
  for (int l = 0; l < 2; l++) {
    for (const char* const* p = lists[l]; p && *p; p++) {
      int fd = open(*p, O_PATH | O_CLOEXEC);
      if (fd < 0) continue;
      struct stat st;
      int dir = fstat(fd, &st) == 0 && S_ISDIR(st.st_mode);
      struct d_ll_path pb = {dir ? (LL_READ_FILE | LL_READ_DIR) : LL_READ_FILE, fd};
      syscall(SYS_landlock_add_rule, rs, 1 /* LANDLOCK_RULE_PATH_BENEATH */, &pb, 0);
      close(fd);
    }
  }
  int ok = syscall(SYS_landlock_restrict_self, rs, 0) == 0;
  close(rs);
  return ok ? 1 : -1;
}

#if __has_include(<linux/seccomp.h>) && __has_include(<linux/filter.h>) && __has_include(<linux/audit.h>)
#include <linux/seccomp.h>
#include <linux/filter.h>
#include <linux/audit.h>
#include <sched.h>
#if defined(__x86_64__)
#define D_AUDIT_ARCH AUDIT_ARCH_X86_64
#elif defined(__aarch64__)
#define D_AUDIT_ARCH AUDIT_ARCH_AARCH64
#endif
#endif

#ifdef D_AUDIT_ARCH
typedef struct SF { struct sock_filter f[160]; int n; } SF;
static void sf_add(SF* s, struct sock_filter x) { if (s->n < 160) s->f[s->n++] = x; }
static void sf_deny(SF* s, int nr, int err) {
  sf_add(s, (struct sock_filter)BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, (uint32_t)nr, 0, 1));
  sf_add(s, (struct sock_filter)BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | (uint32_t)err));
}
static int sb_seccomp(int net, int proc) {
  SF s; s.n = 0;
  sf_add(&s, (struct sock_filter)BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)));
  sf_add(&s, (struct sock_filter)BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, D_AUDIT_ARCH, 1, 0));
  sf_add(&s, (struct sock_filter)BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS)); /* foreign ABI (e.g. int 0x80) */
  sf_add(&s, (struct sock_filter)BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)));
#ifdef __x86_64__
  sf_add(&s, (struct sock_filter)BPF_JUMP(BPF_JMP | BPF_JGE | BPF_K, 0x40000000u, 0, 1)); /* x32 */
  sf_add(&s, (struct sock_filter)BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | ENOSYS));
#endif
  static const int always[] = {
    SYS_bpf, SYS_perf_event_open, SYS_userfaultfd, SYS_kexec_load, SYS_init_module, SYS_finit_module, SYS_delete_module,
    SYS_mount, SYS_umount2, SYS_pivot_root, SYS_chroot, SYS_unshare, SYS_setns, SYS_keyctl, SYS_add_key, SYS_request_key,
    SYS_process_vm_readv, SYS_process_vm_writev,
#ifdef SYS_io_uring_setup
    SYS_io_uring_setup,
#endif
  };
  for (size_t k = 0; k < sizeof always / sizeof always[0]; k++) sf_deny(&s, always[k], EPERM);
  if (!D_SANITIZED) sf_deny(&s, SYS_ptrace, EPERM);
  if (!net) sf_deny(&s, SYS_socket, EACCES);
  if (!proc) {
    sf_deny(&s, SYS_execve, EACCES);
    sf_deny(&s, SYS_execveat, EACCES);
#ifdef SYS_fork
    sf_deny(&s, SYS_fork, EACCES);
    sf_deny(&s, SYS_vfork, EACCES);
#endif
#ifdef SYS_clone3
    sf_deny(&s, SYS_clone3, ENOSYS); /* its flags live in memory; libc falls back to clone */
#endif
    if (!D_SANITIZED) {
      /* clone: only threads (CLONE_THREAD in the first argument) */
      sf_add(&s, (struct sock_filter)BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_clone, 0, 4));
      sf_add(&s, (struct sock_filter)BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[0])));
      sf_add(&s, (struct sock_filter)BPF_JUMP(BPF_JMP | BPF_JSET | BPF_K, CLONE_THREAD, 0, 1));
      sf_add(&s, (struct sock_filter)BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
      sf_add(&s, (struct sock_filter)BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EACCES));
    }
  }
  sf_add(&s, (struct sock_filter)BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
  if (s.n >= 160) return -1;
  struct sock_fprog prog = {(unsigned short)s.n, s.f};
  return prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &prog, 0, 0) == 0 ? 1 : -1;
}
#else
static int sb_seccomp(int net, int proc) { (void)net; (void)proc; return 0; }
#endif

static const char* sb_state(int r) { return r == 2 ? "off (uses fs)" : r > 0 ? "on" : r == 0 ? "unavailable" : "failed"; }
static void sb_report(int bits, int ll, int sc) {
  static const char* const names[] = {"io", "fs", "net", "db", "env", "time", "rand", "proc", "spawn"};
  char eff[80] = "";
  for (int i = 0; i < 9; i++) if (bits & (1 << i)) { if (eff[0]) strcat(eff, ","); strcat(eff, names[i]); }
  fprintf(stderr, "sandbox: uses=%s landlock=%s", eff[0] ? eff : "(pure)", sb_state(ll));
  if (ll == 1) fprintf(stderr, "(abi %d)", ll_abi);
  fprintf(stderr, " seccomp=%s\n", sb_state(sc));
  int fd = open("/etc/passwd", O_RDONLY | O_CLOEXEC);
  fprintf(stderr, "sandbox: read /etc/passwd: %s\n", fd >= 0 ? "allowed" : strerror(errno));
  if (fd >= 0) close(fd);
  fd = open("drift-sandbox-probe.tmp", O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
  fprintf(stderr, "sandbox: create a file: %s\n", fd >= 0 ? "allowed" : strerror(errno));
  if (fd >= 0) { close(fd); unlink("drift-sandbox-probe.tmp"); }
  int so = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
  fprintf(stderr, "sandbox: open a socket: %s\n", so >= 0 ? "allowed" : strerror(errno));
  if (so >= 0) close(so);
  fflush(stderr);
  pid_t p = fork();
  if (p == 0) _exit(0);
  if (p > 0) waitpid(p, NULL, 0);
  fprintf(stderr, "sandbox: fork: %s\n", p >= 0 ? "allowed" : strerror(errno));
}
void d_sandbox(int bits) {
  const char* mode = getenv("DRIFT_SANDBOX");
  if (mode && !strcmp(mode, "off")) return;
  int strict = mode && !strcmp(mode, "strict");
  int fs = bits & (EF_FS | EF_DB), net = bits & (EF_NET | EF_DB), proc = bits & EF_PROC;
  if (net) { http_init_once(); tls_load(); }   /* dlopen OpenSSL while the filesystem is still open */
  prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
  int ll = fs ? 1 : sb_landlock(net != 0);
  int sc = sb_seccomp(net != 0, proc != 0);
  if (getenv("DRIFT_SANDBOX_REPORT")) sb_report(bits, fs ? 2 : ll, sc);
  if (ll < 0 || sc < 0 || (strict && (ll == 0 || sc == 0)))
    panicf("cannot apply the sandbox (landlock %s, seccomp %s); set DRIFT_SANDBOX=off to run unrestricted", sb_state(ll), sb_state(sc));
}
