/* Drift runtime: uniform 16-byte values, reference counting, copy-on-write containers. */
#ifndef DRIFT_RT_H
#define DRIFT_RT_H
#include <stdint.h>
#include <stddef.h>
#include <string.h>
#include <math.h>

enum { K_UNIT, K_BOOL, K_INT, K_FLOAT, K_STR, K_BYTES, K_LIST, K_MAP, K_SET, K_TUPLE, K_ADT, K_FN, K_RANGE, K_HANDLE, K_TASK, K_CHAN };

/* rc > 0: counted; rc == 0: immortal (literals, constants, unit variants). */
typedef struct DObj { int64_t rc; } DObj;
typedef struct V { int64_t k; union { int64_t i; double f; DObj* o; }; } V;

typedef struct DStr { DObj h; int64_t len, cap, nchars; char* p; } DStr;
typedef struct DList { DObj h; int64_t len, cap; V* items; } DList;
typedef struct DEntry { V key, val; uint64_t hash; int64_t live; } DEntry;
typedef struct DMap { DObj h; int64_t len, n, cap; DEntry* e; int64_t* idx; int64_t mask; } DMap;
typedef struct DTuple { DObj h; int64_t n; V items[]; } DTuple;
typedef struct DVariant { const char* name; int32_t n; const char* const* fields; } DVariant;
typedef struct DType { const char* name; int32_t nv; const DVariant* v; int32_t is_enum, bare; V* units; } DType;
typedef struct DAdt { DObj h; DType* t; int32_t tag, n; V f[]; } DAdt;
typedef struct DFn { DObj h; V (*code)(struct DFn*, V*); int32_t arity, nenv; V env[]; } DFn;
typedef struct DRange { DObj h; int64_t lo, hi; } DRange;
/* fd >= 0: a file. db: a database connection (dbk 1 SQLite, 2 PostgreSQL). */
typedef struct DHandle { DObj h; int fd; char* path; int dbk; void* db; } DHandle;
typedef struct DTask { DObj h; V val; } DTask;
typedef struct DChan { DObj h; DList* q; int64_t head; } DChan;
typedef struct DBuf { char* p; int64_t len, cap; } DBuf;
/* Runtime description of a concrete type, for reified generics (`json.decode[T]`).
   k: a K_* kind. num: K_INT 0 Int 1 I32 2 U8 3 U32 4 U64; K_FLOAT 0 Float 1 F32.
   a: element types (List/Set: 1, Map: 2, tuple: n). K_ADT: t and vf[variant][field]. */
typedef struct DTy { int32_t k, num; int64_t n; const struct DTy* const* a; DType* t; const struct DTy* const* const* vf; } DTy;
typedef struct DTest { const char* name; V (*fn)(void); int is_example; const char* file; int line; int effects; } DTest;

extern const char* d_loc_file;
extern int d_loc_line;
#define D_AT(f, l) (d_loc_file = (f), d_loc_line = (l))
#define D_UNIT ((V){.k = K_UNIT, .i = 0})

extern DType d_td_core_Option, d_td_core_Result, d_td_core_Error, d_td_fs_IoError, d_td_fs_Mode,
    d_td_http_HttpError, d_td_http_Request, d_td_http_Response, d_td_json_JsonError,
    d_td_re_Regex, d_td_re_RegexError, d_td_re_Match, d_td_html_Document, d_td_html_Node, d_td_db_DbError;

_Noreturn void d_panic_c(const char* msg);
_Noreturn void d_panic_v(V msg);
_Noreturn void d_unimpl(const char* what);
void d_free(V v);
V d_add_slow(V a, V b);
int d_eq_b(V a, V b);
int d_cmp_b(V a, V b);
uint64_t d_hash(V v);

static inline V d_int(int64_t i) { V v; v.k = K_INT; v.i = i; return v; }
static inline V d_float(double f) { V v; v.k = K_FLOAT; v.f = f; return v; }
static inline V d_bool(int b) { V v; v.k = K_BOOL; v.i = b != 0; return v; }
static inline V d_retain(V v) { if (v.k >= K_STR && v.o->rc > 0) v.o->rc++; return v; }
static inline void d_release(V v) { if (v.k >= K_STR && v.o->rc > 0 && --v.o->rc == 0) d_free(v); }

static inline V d_add(V a, V b) {
  if (a.k == K_INT) { int64_t r; if (__builtin_add_overflow(a.i, b.i, &r)) d_panic_c("integer overflow in `+`"); return d_int(r); }
  if (a.k == K_FLOAT) return d_float(a.f + b.f);
  return d_add_slow(a, b);
}
static inline V d_sub(V a, V b) {
  if (a.k == K_INT) { int64_t r; if (__builtin_sub_overflow(a.i, b.i, &r)) d_panic_c("integer overflow in `-`"); return d_int(r); }
  return d_float(a.f - b.f);
}
static inline V d_mul(V a, V b) {
  if (a.k == K_INT) { int64_t r; if (__builtin_mul_overflow(a.i, b.i, &r)) d_panic_c("integer overflow in `*`"); return d_int(r); }
  return d_float(a.f * b.f);
}
static inline V d_div(V a, V b) {
  if (a.k == K_INT) {
    if (b.i == 0) d_panic_c("division by zero");
    if (b.i == -1 && a.i == INT64_MIN) d_panic_c("integer overflow in `/`");
    return d_int(a.i / b.i);
  }
  return d_float(a.f / b.f);
}
static inline V d_rem(V a, V b) {
  if (a.k == K_INT) {
    if (b.i == 0) d_panic_c("division by zero in `%`");
    if (b.i == -1) return d_int(0);
    return d_int(a.i % b.i);
  }
  return d_float(fmod(a.f, b.f));
}
static inline V d_neg(V a) {
  if (a.k == K_INT) { if (a.i == INT64_MIN) d_panic_c("integer overflow in `-`"); return d_int(-a.i); }
  return d_float(-a.f);
}
static inline V d_op_eq(V a, V b) {
  if (a.k <= K_INT) return d_bool(a.i == b.i);
  if (a.k == K_FLOAT) return d_bool(a.f == b.f);
  V r = d_bool(d_eq_b(a, b)); d_release(a); d_release(b); return r;
}
static inline V d_op_ne(V a, V b) { V r = d_op_eq(a, b); r.i = !r.i; return r; }
static inline int d_cmp_own(V a, V b) {
  if (a.k == K_INT) return (a.i > b.i) - (a.i < b.i);
  int r = d_cmp_b(a, b); d_release(a); d_release(b); return r;
}
static inline V d_op_lt(V a, V b) { return d_bool(d_cmp_own(a, b) < 0); }
static inline V d_op_le(V a, V b) { return d_bool(d_cmp_own(a, b) <= 0); }
static inline V d_op_gt(V a, V b) { return d_bool(d_cmp_own(a, b) > 0); }
static inline V d_op_ge(V a, V b) { return d_bool(d_cmp_own(a, b) >= 0); }

V d_str_new(const char* s, int64_t n);
V d_str_const(const char* s, int64_t n);
V d_bytes_const(const char* s, int64_t n);
void d_buf_init(DBuf* b);
void d_buf_add(DBuf* b, const char* s, int64_t n);
void d_buf_show(DBuf* b, V v, int top);
V d_buf_done(DBuf* b);
void d_show_into(DBuf* b, V v, int top);

V d_tuple(int64_t n, V* items);
V d_list(int64_t n, V* items);
V d_map(int64_t n, V* kv);
V d_adt(DType* t, int tag, int n, V* f);
V d_with(V base, int n, const int* idx, V* vals);
V d_field(V obj, int i);
static inline int64_t d_tag(V v) { return ((DAdt*)v.o)->tag; }
V d_get(V v, int64_t i);
static inline int64_t d_llen(V v) { return ((DList*)v.o)->len; }
V d_list_rest(V v, int64_t n);
V d_take0(V v);
V* d_mut_field(V* p, int i);
V* d_mut_index(V* p, V key, int insert);
V d_index(V base, V idx);
V d_range(V lo, V hi, int inclusive);
int64_t d_iter_len(V it);
V d_iter_at(V it, int64_t i);
V d_fn_new(V (*code)(DFn*, V*), int arity, int nenv, V* env);
V d_call(V f, int n, V* args);
V d_some(V v);
V d_none(void);
V d_ok(V v);
V d_err(V e);
V d_to_error(V e);
V d_task(V v);
void d_immortal(V v);

_Noreturn void d_contract_fail(const char* kind, const char* fn, const char* src);
_Noreturn void d_assert_fail(const char* src, V msg);
_Noreturn void d_assert_eq_fail(const char* src, V l, V r, V msg);
void d_example_fail(const char* file, int line, const char* src, V l, V r);
void d_example_false(const char* file, int line, const char* src);

void d_init(int argc, char** argv);
/* Restrict the process to the effects in `bits` (bit i = check::EFFECTS[i]): Landlock + seccomp. */
void d_sandbox(int bits);
int d_finish_main(V r, int is_result);
int d_run_tests(const DTest* tests, int n, int argc, char** argv);
#endif
