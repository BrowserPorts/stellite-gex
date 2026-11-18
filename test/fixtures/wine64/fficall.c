/* gexcc-win's libffi: ffi_call with mixed integer, floating, pointer and
 * struct-by-value arguments and results, to native function values (a DLL's
 * imports, the program's own variadic one: gex.ffi_thunk's thunks) and to
 * guest ones (GetProcAddress: __gexw_guest_call64); closures refused */
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <windows.h>
#include <ffi.h>

struct pt { int x, y; };
struct big { double a, b, c; };
struct wrapd { double d; };
struct trio { char c[3]; };
__declspec(dllimport) double mix(int8_t, uint16_t, int, int64_t, float, double, const int *);
__declspec(dllimport) struct pt pt_add(struct pt, struct pt);
__declspec(dllimport) struct big big_scale(struct big, double);
__declspec(dllimport) struct wrapd wrap_half(struct wrapd);
__declspec(dllimport) int trio_sum(struct trio, int);
__declspec(dllimport) float fsum(float, float);
__declspec(dllimport) unsigned char uc_add(unsigned char, unsigned char);
__declspec(dllimport) short s_neg(short);
__declspec(dllimport) void set_out(int *, int);
unsigned long long __gexw_ffi_icall_limit(void);

/* the program's own: variable ints and doubles */
static double vsum(int n, ...)
{
    va_list ap;
    double s = 0;
    va_start(ap, n);
    for (int i = 0; i < n; i++) s += (i & 1) ? va_arg(ap, double) : va_arg(ap, int);
    va_end(ap);
    return s;
}

static int fails;
#define CHECK(what, cond) do { if (!(cond)) { printf("fficall: FAIL %s %s\n", tag, what); fails++; } } while (0)

static ffi_type *pt_el[] = { &ffi_type_sint32, &ffi_type_sint32, NULL };
static ffi_type pt_t = { 0, 0, FFI_TYPE_STRUCT, pt_el };
static ffi_type *big_el[] = { &ffi_type_double, &ffi_type_double, &ffi_type_double, NULL };
static ffi_type big_t = { 0, 0, FFI_TYPE_STRUCT, big_el };
static ffi_type *wrapd_el[] = { &ffi_type_double, NULL };
static ffi_type wrapd_t = { 0, 0, FFI_TYPE_STRUCT, wrapd_el };
static ffi_type *trio_el[] = { &ffi_type_sint8, &ffi_type_sint8, &ffi_type_sint8, NULL };
static ffi_type trio_t = { 0, 0, FFI_TYPE_STRUCT, trio_el };

struct fns { void *mix, *pt_add, *big_scale, *wrap_half, *trio_sum, *fsum, *uc_add, *s_neg, *set_out; };

static void run(const char *tag, const struct fns *f)
{
    ffi_cif cif;
    {
        ffi_type *at[] = { &ffi_type_sint8, &ffi_type_uint16, &ffi_type_sint32, &ffi_type_sint64, &ffi_type_float, &ffi_type_double, &ffi_type_pointer };
        int8_t a = -3; uint16_t b = 60000; int c = -100000; int64_t d = 1LL << 40; float e = 0.5f; double g = 0.25; int k = 7;
        const int *p = &k;
        void *av[] = { &a, &b, &c, &d, &e, &g, &p };
        double r = 0;
        CHECK("mix prep", ffi_prep_cif(&cif, FFI_DEFAULT_ABI, 7, &ffi_type_double, at) == FFI_OK);
        ffi_call(&cif, FFI_FN(f->mix), &r, av);
        CHECK("mix", r == -3 + 60000 - 100000 + 1099511627776.0 + 0.75 + 7);
        ffi_call(&cif, FFI_FN(f->mix), &r, av); /* (the cif's thunk, again) */
        CHECK("mix again", r == -3 + 60000 - 100000 + 1099511627776.0 + 0.75 + 7);
    }
    {
        ffi_type *at[] = { &pt_t, &pt_t };
        struct pt a = { 1, 2 }, b = { 30, 40 }, r = { 0, 0 };
        void *av[] = { &a, &b };
        CHECK("pt prep", ffi_prep_cif(&cif, FFI_DEFAULT_ABI, 2, &pt_t, at) == FFI_OK);
        ffi_call(&cif, FFI_FN(f->pt_add), &r, av);
        CHECK("pt_add", r.x == 31 && r.y == 42 && a.x == 1);
    }
    {
        ffi_type *at[] = { &big_t, &ffi_type_double };
        struct big v = { 1, 2, 3 }, r = { 0, 0, 0 };
        double k = 1.5;
        void *av[] = { &v, &k };
        CHECK("big prep", ffi_prep_cif(&cif, FFI_DEFAULT_ABI, 2, &big_t, at) == FFI_OK && big_t.size == 24);
        ffi_call(&cif, FFI_FN(f->big_scale), &r, av);
        CHECK("big_scale", r.a == 1.5 && r.b == 3 && r.c == 4.5 && v.c == 3);
    }
    /* (not to a guest value yet: GexIcall's entry thunk returns a struct
     * that wasm lowers to a double, as here, in XMM0, where Win64 returns
     * any 8-byte struct in RAX; the IR no longer says it was a struct) */
    if (strcmp(tag, "guest")) {
        ffi_type *at[] = { &wrapd_t };
        struct wrapd v = { 5 }, r = { 0 };
        void *av[] = { &v };
        CHECK("wrapd prep", ffi_prep_cif(&cif, FFI_DEFAULT_ABI, 1, &wrapd_t, at) == FFI_OK);
        ffi_call(&cif, FFI_FN(f->wrap_half), &r, av);
        CHECK("wrap_half", r.d == 2.5);
    }
    {
        ffi_type *at[] = { &trio_t, &ffi_type_sint32 };
        struct trio t = { { 1, 2, 3 } };
        int k = 100;
        ffi_arg r = 0;
        void *av[] = { &t, &k };
        CHECK("trio prep", ffi_prep_cif(&cif, FFI_DEFAULT_ABI, 2, &ffi_type_sint32, at) == FFI_OK && trio_t.size == 3);
        ffi_call(&cif, FFI_FN(f->trio_sum), &r, av);
        CHECK("trio_sum", (int)r == 106);
    }
    {
        ffi_type *at[] = { &ffi_type_float, &ffi_type_float };
        float a = 1.25f, b = 2.5f, r = 0;
        void *av[] = { &a, &b };
        ffi_prep_cif(&cif, FFI_DEFAULT_ABI, 2, &ffi_type_float, at);
        ffi_call(&cif, FFI_FN(f->fsum), &r, av);
        CHECK("fsum", r == 3.75f);
    }
    {
        ffi_type *at[] = { &ffi_type_uint8, &ffi_type_uint8 };
        unsigned char a = 200, b = 100;
        ffi_arg r = 12345;
        void *av[] = { &a, &b };
        ffi_prep_cif(&cif, FFI_DEFAULT_ABI, 2, &ffi_type_uint8, at);
        ffi_call(&cif, FFI_FN(f->uc_add), &r, av);
        CHECK("uc_add", r == 44);
    }
    {
        ffi_type *at[] = { &ffi_type_sint16 };
        short a = 5;
        ffi_arg r = 0;
        void *av[] = { &a };
        ffi_prep_cif(&cif, FFI_DEFAULT_ABI, 1, &ffi_type_sint16, at);
        ffi_call(&cif, FFI_FN(f->s_neg), &r, av);
        CHECK("s_neg", (ffi_sarg)r == -5);
    }
    {
        ffi_type *at[] = { &ffi_type_pointer, &ffi_type_sint32 };
        int out = 0, v = 99;
        int *po = &out;
        void *av[] = { &po, &v };
        ffi_prep_cif(&cif, FFI_DEFAULT_ABI, 2, &ffi_type_void, at);
        ffi_call(&cif, FFI_FN(f->set_out), NULL, av);
        CHECK("set_out", out == 99);
    }
}

int main(void)
{
    const char *tag = "main";
    unsigned long long lim = __gexw_ffi_icall_limit();
    struct fns native = { (void *)mix, (void *)pt_add, (void *)big_scale, (void *)wrap_half, (void *)trio_sum,
                          (void *)fsum, (void *)uc_add, (void *)s_neg, (void *)set_out };
    HMODULE h = GetModuleHandleA("ffidll.dll");
    struct fns guest = { 0 };
    void **g = (void **)&guest;
    static const char *names[] = { "mix", "pt_add", "big_scale", "wrap_half", "trio_sum", "fsum", "uc_add", "s_neg", "set_out" };
    for (int i = 0; i < 9; i++) g[i] = (void *)GetProcAddress(h, names[i]);
    CHECK("native values", (uintptr_t)native.mix < lim && (uintptr_t)vsum < lim);
    CHECK("guest values", guest.mix && (uintptr_t)guest.mix >= lim);
    run("native", &native);
    run("guest", &guest);
    {
        /* a variadic native call: (int n, ...) with ints and doubles */
        ffi_cif cif;
        ffi_type *at[] = { &ffi_type_sint32, &ffi_type_sint32, &ffi_type_double, &ffi_type_sint32, &ffi_type_double };
        int n = 4, a = 10, c = -3;
        double b = 0.5, d = 0.25, r = 0;
        void *av[] = { &n, &a, &b, &c, &d };
        tag = "var";
        CHECK("prep", ffi_prep_cif_var(&cif, FFI_DEFAULT_ABI, 1, 5, &ffi_type_double, at) == FFI_OK);
        ffi_call(&cif, FFI_FN(vsum), &r, av);
        CHECK("vsum", r == 7.75);
    }
    {
        ffi_cif cif;
        void *code;
        ffi_closure *cl = ffi_closure_alloc(sizeof(ffi_closure), &code);
        tag = "closure";
        ffi_prep_cif(&cif, FFI_DEFAULT_ABI, 0, &ffi_type_void, NULL);
        CHECK("refused", cl && ffi_prep_closure_loc(cl, &cif, NULL, NULL, code) == FFI_BAD_ABI);
        ffi_closure_free(cl);
    }
    printf("fficall: %s %d\n", fails ? "failed" : "ok", fails);
    return fails != 0;
}
