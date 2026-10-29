#ifndef SCARLET_FENV_H
#define SCARLET_FENV_H
/* Only the rounding-mode API implemented by the AArch64 native companion is exposed. */
#if defined(__aarch64__)
/* Native AArch64 FPCR/FPSR snapshot layout, also used by LLVM's internal
 * formatting code. Environment save/restore functions are not yet exported. */
typedef struct { unsigned int control, status; } fenv_t;
#define FE_DFL_ENV ((const fenv_t *)-1)
#endif
#define FE_TONEAREST  0x000000
#define FE_UPWARD     0x400000
#define FE_DOWNWARD   0x800000
#define FE_TOWARDZERO 0xc00000
#ifdef __cplusplus
extern "C" {
#endif
int fegetround(void);
int fesetround(int);
#ifdef __cplusplus
}
#endif
#endif
