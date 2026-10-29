#include <errno.h>
#include <ctype.h>
#include <locale.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/* Every supported category is C/POSIX, but locale objects still have distinct
 * ownership and thread selection. Do not use ELF TLS in a loadable companion. */
struct scarlet_locale { unsigned char c_locale; };
static pthread_key_t selected_locale;
static pthread_once_t locale_once = PTHREAD_ONCE_INIT;
static int locale_key_error;

static void create_locale_key(void) {
    locale_key_error = pthread_key_create(&selected_locale, NULL);
}

static int supported_name(const char *name) {
    return !strcmp(name, "C") || !strcmp(name, "POSIX");
}

static int supported_environment(int category) {
    static const char *const variables[] = {
        "LC_CTYPE", "LC_NUMERIC", "LC_TIME", "LC_COLLATE", "LC_MONETARY", "LC_MESSAGES"
    };
    const char *name = getenv("LC_ALL");
    if (!name || !*name) name = getenv(variables[category]);
    if (!name || !*name) name = getenv("LANG");
    return !name || !*name || supported_name(name);
}

static int supported(int mask, const char *name) {
    if (*name) return supported_name(name);
    for (int category = LC_CTYPE; category < LC_ALL; ++category)
        if ((mask & (1 << category)) && !supported_environment(category)) return 0;
    return 1;
}

char* setlocale(int category, const char* name) {
    static char c_locale[] = "C";
    if (category < LC_CTYPE || category > LC_ALL) {
        errno = EINVAL;
        return NULL;
    }
    if (!name || supported(category == LC_ALL ? LC_ALL_MASK : 1 << category, name)) return c_locale;
    // Native locale support currently comprises C/POSIX only.
    errno = ENOENT;
    return NULL;
}

locale_t newlocale(int mask, const char *name, locale_t base) {
    if ((mask & ~LC_ALL_MASK) || !name || base == LC_GLOBAL_LOCALE) {
        errno = EINVAL;
        return NULL;
    }
    if (!supported(mask, name)) {
        errno = ENOENT;
        return NULL;
    }
    /* No category changes representation in this C/POSIX-only implementation.
     * On success POSIX transfers ownership of base to the returned handle. */
    if (base) return base;
    locale_t result = malloc(sizeof(*result));
    if (!result) { errno = ENOMEM; return NULL; }
    result->c_locale = 1;
    return result;
}

locale_t duplocale(locale_t source) {
    if (!source) { errno = EINVAL; return NULL; }
    return newlocale(LC_ALL_MASK, "C", NULL);
}

void freelocale(locale_t locale) {
    if (locale != LC_GLOBAL_LOCALE) free(locale);
}

locale_t uselocale(locale_t locale) {
    int error = pthread_once(&locale_once, create_locale_key);
    if (!error) error = locale_key_error;
    if (error) { errno = error; return NULL; }
    locale_t old = pthread_getspecific(selected_locale);
    if (!old) old = LC_GLOBAL_LOCALE;
    if (locale) {
        error = pthread_setspecific(selected_locale, locale);
        if (error) { errno = error; return NULL; }
    }
    return old;
}

/* The locale argument denotes a supported C/POSIX object, so these operations
 * have the same byte ordering, classification and numeric grammar as libc. */
#define CLASSIFY_L(name) \
    int name##_l(int value, locale_t locale) { (void)locale; return name(value); }
CLASSIFY_L(isalnum) CLASSIFY_L(isalpha) CLASSIFY_L(isblank) CLASSIFY_L(iscntrl)
CLASSIFY_L(isdigit) CLASSIFY_L(isgraph) CLASSIFY_L(islower) CLASSIFY_L(isprint)
CLASSIFY_L(ispunct) CLASSIFY_L(isspace) CLASSIFY_L(isupper) CLASSIFY_L(isxdigit)
CLASSIFY_L(tolower) CLASSIFY_L(toupper)
#undef CLASSIFY_L

int strcoll(const char *left, const char *right) { return strcmp(left, right); }
int strcoll_l(const char *left, const char *right, locale_t locale) {
    (void)locale;
    return strcoll(left, right);
}
size_t strxfrm(char *destination, const char *source, size_t capacity) {
    size_t length = strlen(source);
    if (capacity) memcpy(destination, source, length < capacity ? length + 1 : capacity);
    return length;
}
size_t strxfrm_l(char *destination, const char *source, size_t capacity, locale_t locale) {
    (void)locale;
    return strxfrm(destination, source, capacity);
}
#define FLOAT_L(type, name) \
    type name##_l(const char *text, char **end, locale_t locale) { \
        (void)locale; return name(text, end); \
    }
FLOAT_L(float, strtof) FLOAT_L(double, strtod) FLOAT_L(long double, strtold)
#undef FLOAT_L
long long strtoll_l(const char *text, char **end, int base, locale_t locale) {
    (void)locale; return strtoll(text, end, base);
}
unsigned long long strtoull_l(const char *text, char **end, int base, locale_t locale) {
    (void)locale; return strtoull(text, end, base);
}
size_t strftime_l(char *output, size_t capacity, const char *format,
                  const struct tm *time, locale_t locale) {
    (void)locale; return strftime(output, capacity, format, time);
}
