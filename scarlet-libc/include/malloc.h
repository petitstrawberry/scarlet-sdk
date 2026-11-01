#ifndef SCARLET_MALLOC_H
#define SCARLET_MALLOC_H
#include <stddef.h>
#include <stdlib.h>
#ifdef __cplusplus
extern "C" {
#endif
size_t malloc_usable_size(void *);
#ifdef __cplusplus
}
#endif
#endif
