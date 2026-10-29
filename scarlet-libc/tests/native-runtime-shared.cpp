// Exercise the installed C++ runtime through a loadable native library.
#include <iomanip>
#include <locale>
#include <mutex>
#include <sstream>
#include <string>
#include <thread>
#include <vector>
#include <cerrno>
#include <cstring>
#include <locale.h>
#include <time.h>
#include <cstdio>
#include <cstdlib>
#include <fenv.h>

extern "C" __attribute__((visibility("default"))) int scarlet_runtime_shared_probe() {
    char formatted[64];
    if (std::snprintf(formatted, sizeof(formatted), "%+.2f %a %.3e", 1.25, 1.5, 1.25) != 24 ||
        std::strcmp(formatted, "+1.25 0x1.8p+0 1.250e+00")) return 14;
    if (std::snprintf(formatted, 4, "%.5f", 1.25) != 7 || std::strcmp(formatted, "1.2")) return 16;
    if (std::snprintf(formatted, sizeof(formatted), "%#.3Lg", 1.25L) != 4 ||
        std::strcmp(formatted, "1.25")) return 17;
    int rounding = fegetround();
    if (fesetround(FE_DOWNWARD)) return 18;
    int count = std::snprintf(formatted, sizeof(formatted), "%.0f", 2.75);
    int downward = !std::strcmp(formatted, "2");
    if (fesetround(rounding) || count != 1 || !downward) return 19;
    std::ostringstream output;
    output.imbue(std::locale::classic());
    output << std::hex << 255 << ' ' << std::fixed << std::setprecision(2) << 1.25;
    if (output.str() != "ff 1.25") return 1;
    std::istringstream input("ff 1.25");
    unsigned value = 0;
    double fraction = 0;
    input >> std::hex >> value >> fraction;
    if (!input || value != 255 || fraction != 1.25) return 2;
    if (!std::isalpha('A', std::locale::classic()) || std::isdigit('A', std::locale::classic())) return 3;
    locale_t locale = newlocale(LC_ALL_MASK, "C", nullptr);
    if (!locale) return 4;
    locale_t copy = duplocale(locale);
    if (!copy || copy == locale) return 5;
    locale_t previous = uselocale(locale);
    if (!previous || uselocale(nullptr) != locale) return 6;
    errno = 0;
    if (newlocale(LC_ALL_MASK, "scarlet-no-such-locale", locale) || errno != ENOENT) return 7;
    if (uselocale(nullptr) != locale) return 8;
    char key[3] = {};
    if (strxfrm_l(key, "abcd", sizeof(key), locale) != 4 || std::memcmp(key, "abc", 3)) return 9;
    struct tm time = {};
    time.tm_year = 126; time.tm_mon = 9; time.tm_mday = 30; time.tm_wday = 5;
    char date[64];
    if (strftime_l(date, sizeof(date), "%A %F", &time, locale) != 17 ||
        std::strcmp(date, "Friday 2026-10-30")) return 10;
    // NUL needs its own byte; an exact-length output buffer must report failure.
    if (strftime(date, 10, "%F", &time) != 0) return 11;
    time.tm_gmtoff = -19800; time.tm_zone = "TEST";
    if (strftime(date, sizeof(date), "%z %Z", &time) != 10 || std::strcmp(date, "-0530 TEST")) return 15;
    std::mutex lock;
    std::vector<int> results(4, -1);
    std::vector<std::thread> threads;
    for (int index = 0; index < 4; ++index) {
        threads.emplace_back([&, index] {
            if (uselocale(nullptr) != LC_GLOBAL_LOCALE) return;
            locale_t old = uselocale(copy);
            std::ostringstream stream;
            stream << std::hex << (index + 16);
            std::lock_guard<std::mutex> guard(lock);
            if (old == LC_GLOBAL_LOCALE && uselocale(nullptr) == copy && stream.str().size() == 2)
                results[index] = index;
            uselocale(old);
        });
    }
    for (auto &thread : threads) thread.join();
    if (uselocale(nullptr) != locale) return 12;
    uselocale(previous);
    freelocale(copy);
    freelocale(locale);
    for (int index = 0; index < 4; ++index) if (results[index] != index) return 13;
    return 0;
}
