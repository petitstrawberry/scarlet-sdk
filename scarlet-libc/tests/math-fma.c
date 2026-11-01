#include <fenv.h>
#include <math.h>
#include <stdio.h>

static unsigned long flags(void)
{
    unsigned long result;
    __asm__ volatile("mrs %0, fpsr" : "=r"(result) : : "memory");
    return result;
}
static void set_flags(unsigned long value)
{
    __asm__ volatile("msr fpsr, %0" : : "r"(value) : "memory");
}

/* Volatile function pointers exercise the actual exported ABI, including
 * rounding modes and exception flags, without builtin constant folding. */
int main(void)
{
    double (*volatile fused)(double, double, double) = fma;
    float (*volatile fusedf)(float, float, float) = fmaf;
    long (*volatile integer)(float) = lrintf;
    int initial = fegetround();
    unsigned long initial_flags = flags();
    int failures = 0;
    fesetround(FE_TONEAREST);
    failures += fused(0x1.0000000000001p0, 0x1.fffffffffffffp0, -2.0)
        != 0x1.ffffffffffffep-53;
    failures += fusedf(0x1.000002p0f, 0x1.fffffep0f, -2.0f)
        != 0x1.fffffcp-24f;
    const int modes[] = { FE_TONEAREST, FE_UPWARD, FE_DOWNWARD, FE_TOWARDZERO };
    const long positive[] = { 2, 3, 2, 2 };
    const long negative[] = { -2, -2, -3, -2 };
    for (unsigned i = 0; i < 4; ++i) {
        fesetround(modes[i]);
        set_flags(0);
        failures += integer(2.5f) != positive[i];
        failures += !(flags() & 16); /* FPSR IXC */
        failures += integer(-2.5f) != negative[i];
        failures += fegetround() != modes[i];
    }
    fesetround(FE_TONEAREST);
    set_flags(0);
    failures += !isnan(fusedf(INFINITY, 0.0f, 1.0f));
    failures += !(flags() & 1); /* FPSR IOC */
    fesetround(initial);
    set_flags(initial_flags);
    printf("SCARLET_MATH_FMA failures=%d\n", failures);
    return failures ? 1 : 0;
}
