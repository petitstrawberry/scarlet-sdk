// Full C-locale printf conversions from the SDK's pinned LLVM libc core.
// The Rust archive is built with external-format when this provider is used.
#include <errno.h>
#include <limits.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
// Formatting reads the real rounding mode but does not implement libm's
// optional errno/exception policy. Leave the standalone libm contract alone.
#define LIBC_MATH_HAS_NO_EXCEPT
#define LIBC_MATH_HAS_NO_ERRNO
#define LIBC_COPT_PRINTF_DISABLE_STRERROR
#include "src/stdio/printf_core/printf_main.h"

extern "C" int vsnprintf(char *output, size_t capacity, const char *format, va_list list) {
    if (!format || (capacity && !output)) { errno = EINVAL; return -1; }
    namespace core = __llvm_libc::printf_core;
    __llvm_libc::internal::ArgList arguments(list);
    core::WriteBuffer<core::WriteMode::FILL_BUFF_AND_DROP_OVERFLOW> buffer(
        output, capacity ? capacity - 1 : 0);
    core::Writer writer(buffer);
    int result = core::printf_main(&writer, format, arguments);
    if (capacity) output[buffer.buff_cur] = '\0';
    return result;
}

extern "C" int snprintf(char *output, size_t capacity, const char *format, ...) {
    va_list list;
    va_start(list, format);
    int result = vsnprintf(output, capacity, format, list);
    va_end(list);
    return result;
}

extern "C" int vsprintf(char *output, const char *format, va_list list) {
    return vsnprintf(output, SIZE_MAX, format, list);
}

extern "C" int sprintf(char *output, const char *format, ...) {
    va_list list;
    va_start(list, format);
    int result = vsprintf(output, format, list);
    va_end(list);
    return result;
}

extern "C" int vasprintf(char **output, const char *format, va_list list) {
    if (!output) { errno = EINVAL; return -1; }
    *output = nullptr;
    va_list count;
    va_copy(count, list);
    int length = vsnprintf(nullptr, 0, format, count);
    va_end(count);
    if (length < 0) return -1;
    char *buffer = static_cast<char *>(malloc(static_cast<size_t>(length) + 1));
    if (!buffer) { errno = ENOMEM; return -1; }
    int result = vsnprintf(buffer, static_cast<size_t>(length) + 1, format, list);
    if (result < 0 || result != length) { free(buffer); return -1; }
    *output = buffer;
    return result;
}

extern "C" int asprintf(char **output, const char *format, ...) {
    va_list list;
    va_start(list, format);
    int result = vasprintf(output, format, list);
    va_end(list);
    return result;
}

extern "C" int vfprintf(FILE *stream, const char *format, va_list list) {
    if (!stream) { errno = EINVAL; return -1; }
    char *buffer = nullptr;
    int result = vasprintf(&buffer, format, list);
    if (result < 0) return result;
    size_t written = fwrite(buffer, 1, static_cast<size_t>(result), stream);
    free(buffer);
    return written == static_cast<size_t>(result) ? result : -1;
}

extern "C" int fprintf(FILE *stream, const char *format, ...) {
    va_list list;
    va_start(list, format);
    int result = vfprintf(stream, format, list);
    va_end(list);
    return result;
}

extern "C" int vprintf(const char *format, va_list list) { return vfprintf(stdout, format, list); }

extern "C" int printf(const char *format, ...) {
    va_list list;
    va_start(list, format);
    int result = vprintf(format, list);
    va_end(list);
    return result;
}
