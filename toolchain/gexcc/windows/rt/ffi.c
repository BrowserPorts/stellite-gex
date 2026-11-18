/* gexcc-win's libffi: libffi 3's ffi_prep_cif and ffi_call for native
 * wasm64 modules under gex (include-rt/ffi.h says what reaches what).
 *
 * A native callee's wasm type follows clang's WebAssembly C ABI as gexcc-win
 * builds (LLP64 lowered: long is i32, pointers i64): integers of 32 bits
 * or fewer are i32, 64-bit ones and pointers i64, float f32, double and long
 * double f64. A struct whose one element (recursively) is a scalar of the
 * struct's size goes as that scalar; any other struct by value goes as a
 * pointer to a copy, and one returned goes through a pointer the caller
 * passes first (the function then returns nothing). A variadic call's
 * variable arguments go in a buffer (each at its alignment, at least 4,
 * every one taking a multiple of 4 bytes) whose pointer follows the fixed
 * ones.
 *
 * A guest callee is called as Win64 calls: one 8-byte slot per argument, a
 * struct of 1, 2, 4 or 8 bytes in its slot and any other as a pointer to a
 * copy, a returned struct of another size through a pointer in the first
 * slot. */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ffi.h>

__attribute__((import_module("gex"), import_name("ffi_thunk")))
unsigned long long __gexw_ffi_thunk(const char *sig, unsigned int len);
unsigned long long __gexw_ffi_icall_limit(void);
uint64_t __gexw_guest_call64(uint64_t target, const uint64_t *slots, uint32_t n, uint32_t fp);

#define SCALAR(name, sz, t) ffi_type ffi_type_##name = { sz, sz, t, NULL };
SCALAR(void, 1, FFI_TYPE_VOID)
SCALAR(uint8, 1, FFI_TYPE_UINT8)
SCALAR(sint8, 1, FFI_TYPE_SINT8)
SCALAR(uint16, 2, FFI_TYPE_UINT16)
SCALAR(sint16, 2, FFI_TYPE_SINT16)
SCALAR(uint32, 4, FFI_TYPE_UINT32)
SCALAR(sint32, 4, FFI_TYPE_SINT32)
SCALAR(uint64, 8, FFI_TYPE_UINT64)
SCALAR(sint64, 8, FFI_TYPE_SINT64)
SCALAR(float, 4, FFI_TYPE_FLOAT)
SCALAR(double, 8, FFI_TYPE_DOUBLE)
SCALAR(longdouble, 8, FFI_TYPE_DOUBLE)
SCALAR(pointer, 8, FFI_TYPE_POINTER)

/* a struct's size and alignment, from its elements, when it has none yet */
static ffi_status init_type(ffi_type *t)
{
    size_t size = 0;
    unsigned short align = 1;
    ffi_type **e;

    switch (t->type)
    {
    case FFI_TYPE_VOID: case FFI_TYPE_INT: case FFI_TYPE_FLOAT: case FFI_TYPE_DOUBLE:
    case FFI_TYPE_UINT8: case FFI_TYPE_SINT8: case FFI_TYPE_UINT16: case FFI_TYPE_SINT16:
    case FFI_TYPE_UINT32: case FFI_TYPE_SINT32: case FFI_TYPE_UINT64: case FFI_TYPE_SINT64:
    case FFI_TYPE_POINTER:
        return t->size ? FFI_OK : FFI_BAD_TYPEDEF;
    case FFI_TYPE_STRUCT:
        break;
    default:
        return FFI_BAD_TYPEDEF;
    }
    if (!t->elements || !t->elements[0]) return FFI_BAD_TYPEDEF;
    if (t->size && t->alignment) return FFI_OK;
    for (e = t->elements; *e; e++)
    {
        if ((*e)->type == FFI_TYPE_VOID || init_type(*e) != FFI_OK) return FFI_BAD_TYPEDEF;
        size = (size + (*e)->alignment - 1) & ~((size_t)(*e)->alignment - 1);
        size += (*e)->size;
        if ((*e)->alignment > align) align = (*e)->alignment;
    }
    t->size = (size + align - 1) & ~((size_t)align - 1);
    t->alignment = align;
    return FFI_OK;
}

ffi_status ffi_get_struct_offsets(ffi_abi abi, ffi_type *t, size_t *offsets)
{
    size_t off = 0;
    ffi_type **e;
    if (abi <= FFI_FIRST_ABI || abi >= FFI_LAST_ABI) return FFI_BAD_ABI;
    if (t->type != FFI_TYPE_STRUCT || init_type(t) != FFI_OK) return FFI_BAD_TYPEDEF;
    for (e = t->elements; *e; e++)
    {
        off = (off + (*e)->alignment - 1) & ~((size_t)(*e)->alignment - 1);
        if (offsets) *offsets++ = off;
        off += (*e)->size;
    }
    return FFI_OK;
}

/* the scalar a struct goes as (its one element's, recursively, when that is
 * the struct's size), the type itself for a scalar, or NULL */
static ffi_type *as_scalar(ffi_type *t)
{
    while (t->type == FFI_TYPE_STRUCT)
    {
        if (!t->elements[0] || t->elements[1] || t->elements[0]->size != t->size) return NULL;
        t = t->elements[0];
    }
    return t;
}

static char wasm_letter(const ffi_type *t)
{
    switch (t->type)
    {
    case FFI_TYPE_VOID: return 'v';
    case FFI_TYPE_FLOAT: return 'f';
    case FFI_TYPE_DOUBLE: return 'd';
    case FFI_TYPE_UINT64: case FFI_TYPE_SINT64: case FFI_TYPE_POINTER: return 'I';
    default: return 'i';
    }
}

/* the integer at `p` of type `t`, extended to 64 bits as its signedness says */
static uint64_t load_int(const ffi_type *t, const void *p)
{
    switch (t->type)
    {
    case FFI_TYPE_UINT8: return *(const uint8_t *)p;
    case FFI_TYPE_SINT8: return (uint64_t)(int64_t)*(const int8_t *)p;
    case FFI_TYPE_UINT16: return *(const uint16_t *)p;
    case FFI_TYPE_SINT16: return (uint64_t)(int64_t)*(const int16_t *)p;
    case FFI_TYPE_UINT32: return *(const uint32_t *)p;
    case FFI_TYPE_INT: case FFI_TYPE_SINT32: return (uint64_t)(int64_t)*(const int32_t *)p;
    default: { uint64_t v; memcpy(&v, p, 8); return v; }
    }
}

static int is_integer(const ffi_type *t)
{
    return t->type != FFI_TYPE_VOID && t->type != FFI_TYPE_FLOAT && t->type != FFI_TYPE_DOUBLE && t->type != FFI_TYPE_STRUCT;
}

static ffi_status prep(ffi_cif *cif, ffi_abi abi, unsigned nfixed, unsigned nargs, ffi_type *rtype, ffi_type **atypes)
{
    unsigned i;
    if (abi <= FFI_FIRST_ABI || abi >= FFI_LAST_ABI) return FFI_BAD_ABI;
    if (nfixed > nargs || init_type(rtype) != FFI_OK) return FFI_BAD_TYPEDEF;
    for (i = 0; i < nargs; i++)
        if (!atypes[i] || atypes[i]->type == FFI_TYPE_VOID || init_type(atypes[i]) != FFI_OK) return FFI_BAD_TYPEDEF;
    /* (a variable argument as C promotes it: no float, no integer narrower than int) */
    for (i = nfixed; i < nargs; i++)
    {
        ffi_type *t = atypes[i];
        if (t->type == FFI_TYPE_FLOAT || ((t->type == FFI_TYPE_UINT8 || t->type == FFI_TYPE_SINT8 || t->type == FFI_TYPE_UINT16 || t->type == FFI_TYPE_SINT16)))
            return FFI_BAD_ARGTYPE;
    }
    cif->abi = abi;
    cif->nargs = nargs;
    cif->nfixed = nfixed;
    cif->arg_types = atypes;
    cif->rtype = rtype;
    cif->bytes = 8 * nargs;
    cif->flags = 0;
    cif->thunk = 0;
    return FFI_OK;
}

ffi_status ffi_prep_cif(ffi_cif *cif, ffi_abi abi, unsigned int nargs, ffi_type *rtype, ffi_type **atypes)
{
    return prep(cif, abi, nargs, nargs, rtype, atypes);
}

ffi_status ffi_prep_cif_var(ffi_cif *cif, ffi_abi abi, unsigned int nfixedargs, unsigned int ntotalargs,
                            ffi_type *rtype, ffi_type **atypes)
{
    ffi_status s = prep(cif, abi, nfixedargs, ntotalargs, rtype, atypes);
    if (s == FFI_OK) cif->flags = 1;
    return s;
}

/* the value at `r` (`t`'s, as the callee left it in its own width) into
 * libffi's rvalue: an integer narrower than ffi_arg widened to it */
static void store_result(const ffi_type *rt, const ffi_type *s, void *rvalue, const void *r)
{
    if (rt->type == FFI_TYPE_VOID) return;
    if (rt->type == FFI_TYPE_STRUCT) { memcpy(rvalue, r, rt->size); return; }
    if (s->type == FFI_TYPE_FLOAT) { memcpy(rvalue, r, 4); return; }
    if (s->type == FFI_TYPE_DOUBLE) { memcpy(rvalue, r, 8); return; }
    *(ffi_arg *)rvalue = load_int(s, r);
}

static void call_guest(ffi_cif *cif, uint64_t fn, void *rvalue, void **avalue)
{
    ffi_type *rt = cif->rtype;
    size_t rs = rt->size;
    int sret = rt->type == FFI_TYPE_STRUCT && rs != 1 && rs != 2 && rs != 4 && rs != 8;
    unsigned n = cif->nargs + sret, k = 0, i;
    uint64_t *slots = __builtin_alloca(8 * (n ? n : 1)), r;
    int fp = rt->type == FFI_TYPE_FLOAT || rt->type == FFI_TYPE_DOUBLE;

    if (sret) slots[k++] = (uint64_t)(uintptr_t)rvalue;
    for (i = 0; i < cif->nargs; i++, k++)
    {
        ffi_type *t = cif->arg_types[i];
        size_t ts = t->size;
        slots[k] = 0;
        if (t->type == FFI_TYPE_STRUCT)
        {
            if (ts == 1 || ts == 2 || ts == 4 || ts == 8) memcpy(&slots[k], avalue[i], ts);
            else
            {
                void *copy = __builtin_alloca_with_align(ts, 128);
                memcpy(copy, avalue[i], ts);
                slots[k] = (uint64_t)(uintptr_t)copy;
            }
        }
        else if (t->type == FFI_TYPE_FLOAT) memcpy(&slots[k], avalue[i], 4);
        else if (t->type == FFI_TYPE_DOUBLE) memcpy(&slots[k], avalue[i], 8);
        else slots[k] = load_int(t, avalue[i]);
    }
    r = __gexw_guest_call64(fn, slots, n, fp);
    if (sret) return;
    if (rt->type == FFI_TYPE_STRUCT) { memcpy(rvalue, &r, rs); return; }
    store_result(rt, rt, rvalue, &r);
}

static void call_native(ffi_cif *cif, uint64_t fn, void *rvalue, void **avalue)
{
    ffi_type *rt = cif->rtype, *rs = as_scalar(rt);
    int sret = !rs, var = cif->flags & 1;
    unsigned np = sret + cif->nfixed + var, k = 0, i;
    uint64_t *tmp = __builtin_alloca(8 * (np ? np : 1));
    void **av = __builtin_alloca(sizeof(void *) * (np ? np : 1));
    uint64_t rbuf[2] = { 0, 0 };
    void (*thunk)(uint64_t, void **, void *);

    if (!cif->thunk)
    {
        char *sig = __builtin_alloca(np + 3);
        unsigned m = 0;
        if (sret) sig[m++] = 'I';
        for (i = 0; i < cif->nfixed; i++)
        {
            ffi_type *s = as_scalar(cif->arg_types[i]);
            sig[m++] = s ? wasm_letter(s) : 'I';
        }
        if (var) sig[m++] = 'I';
        sig[m++] = ':';
        sig[m++] = sret ? 'v' : wasm_letter(rs);
        cif->thunk = __gexw_ffi_thunk(sig, m);
    }
    if (sret) { tmp[k] = (uint64_t)(uintptr_t)rvalue; av[k] = &tmp[k]; k++; }
    for (i = 0; i < cif->nfixed; i++, k++)
    {
        ffi_type *t = cif->arg_types[i], *s = as_scalar(t);
        if (!s)
        {
            void *copy = __builtin_alloca_with_align(t->size, 128);
            memcpy(copy, avalue[i], t->size);
            tmp[k] = (uint64_t)(uintptr_t)copy;
            av[k] = &tmp[k];
        }
        else if (wasm_letter(s) == 'i')
        {
            tmp[k] = load_int(s, avalue[i]);
            av[k] = &tmp[k];
        }
        else av[k] = avalue[i];
    }
    if (var)
    {
        /* clang's WebAssembly va_list: each at its alignment (at least 4),
         * each taking a multiple of 4 bytes; a struct that is not a scalar
         * as a pointer to a copy */
        size_t size = 0, off = 0;
        unsigned char *buf;
        for (i = cif->nfixed; i < cif->nargs; i++)
        {
            ffi_type *t = cif->arg_types[i];
            size_t ts = as_scalar(t) ? t->size : 8, ta = as_scalar(t) ? t->alignment : 8;
            if (ta < 4) ta = 4;
            size = (size + ta - 1) & ~(ta - 1);
            size += (ts + 3) & ~(size_t)3;
        }
        buf = __builtin_alloca_with_align(size ? size : 8, 128);
        for (i = cif->nfixed; i < cif->nargs; i++)
        {
            ffi_type *t = cif->arg_types[i];
            size_t ts = as_scalar(t) ? t->size : 8, ta = as_scalar(t) ? t->alignment : 8;
            if (ta < 4) ta = 4;
            off = (off + ta - 1) & ~(ta - 1);
            if (as_scalar(t)) memcpy(buf + off, avalue[i], ts);
            else
            {
                void *copy = __builtin_alloca_with_align(t->size, 128);
                uint64_t p = (uint64_t)(uintptr_t)copy;
                memcpy(copy, avalue[i], t->size);
                memcpy(buf + off, &p, 8);
            }
            off += (ts + 3) & ~(size_t)3;
        }
        tmp[k] = (uint64_t)(uintptr_t)buf;
        av[k] = &tmp[k];
        k++;
    }
    thunk = (void (*)(uint64_t, void **, void *))(uintptr_t)cif->thunk;
    thunk(fn, av, rbuf);
    if (!sret) store_result(rt, rs, rvalue, rbuf);
}

void ffi_call(ffi_cif *cif, void (*fn)(void), void *rvalue, void **avalue)
{
    uint64_t f = (uint64_t)(uintptr_t)fn;
    uint64_t rdummy[4];
    /* (libffi allows a null rvalue for a result the caller ignores) */
    if (!rvalue && cif->rtype->type != FFI_TYPE_VOID)
        rvalue = cif->rtype->size <= sizeof(rdummy) ? (void *)rdummy : __builtin_alloca_with_align(cif->rtype->size, 128);
    if (f < __gexw_ffi_icall_limit()) call_native(cif, f, rvalue, avalue);
    else call_guest(cif, f, rvalue, avalue);
}

void *ffi_closure_alloc(size_t size, void **code)
{
    void *c = calloc(1, size < sizeof(ffi_closure) ? sizeof(ffi_closure) : size);
    if (code) *code = NULL;
    return c;
}

void ffi_closure_free(void *closure)
{
    free(closure);
}

ffi_status ffi_prep_closure_loc(ffi_closure *closure, ffi_cif *cif,
                                void (*fun)(ffi_cif *, void *, void **, void *), void *user_data, void *codeloc)
{
    static int said;
    (void)closure; (void)cif; (void)fun; (void)user_data; (void)codeloc;
    if (!said++) fprintf(stderr, "libffi (gexcc-win): closures are not supported yet (gex mints no function values for them), FFI_BAD_ABI\n");
    return FFI_BAD_ABI;
}

ffi_status ffi_prep_closure(ffi_closure *closure, ffi_cif *cif,
                            void (*fun)(ffi_cif *, void *, void **, void *), void *user_data)
{
    return ffi_prep_closure_loc(closure, cif, fun, user_data, closure);
}
