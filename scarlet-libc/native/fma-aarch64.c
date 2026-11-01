#include <math.h>

#if !defined(__aarch64__)
#error "The native Scarlet FMA runtime requires AArch64"
#endif

/* One hardware operation preserves fused rounding, FPCR and FPSR semantics.
 * Keeping the instruction explicit also prevents a builtin lowering from
 * recursively calling the C ABI function being defined here. */
double fma(double x, double y, double z)
{
    double result;
    __asm__ volatile("fmadd %d0, %d1, %d2, %d3"
        : "=w"(result) : "w"(x), "w"(y), "w"(z) : "memory");
    return result;
}

float fmaf(float x, float y, float z)
{
    float result;
    __asm__ volatile("fmadd %s0, %s1, %s2, %s3"
        : "=w"(result) : "w"(x), "w"(y), "w"(z) : "memory");
    return result;
}
