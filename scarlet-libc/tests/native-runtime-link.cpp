// Installed SDK link check. Runtime behavior is verified separately in Scarlet.
#include <filesystem>
#include <charconv>
#include <cstdio>
#include <string>
#include <thread>
#include <vector>
#include <math.h>
#include <malloc.h>
#include <unistd.h>

int main() {
    long (*volatile cpus)(int) = sysconf;
    int (*volatile exponent)(float) = ilogbf;
    size_t (*volatile capacity)(void*) = malloc_usable_size;
    void* allocation = malloc(19);
    if (!allocation || capacity(allocation) < 19 || exponent(0x1p-149f) != -149 || cpus(_SC_NPROCESSORS_ONLN) < 1) return 2;
    free(allocation);
    char number[64];
    const double value = 0.12345678912345678;
    auto formatted = std::to_chars(number, number + sizeof(number) - 1, value, std::chars_format::general, 17);
    if (formatted.ec != std::errc{}) return 3;
    *formatted.ptr = '\0';
    char* end = nullptr;
    if (strtod(number, &end) != value || end != formatted.ptr) return 4;
    std::filesystem::path path("/tmp/scarlet-sdk-check");
    std::string name = path.filename().string();
    std::vector<int> values { 1, 2, 3 };
    std::thread thread([&values] { values[0] = 4; });
    thread.join();
    float (*volatile fused)(float, float, float) = fmaf;
    long (*volatile integer)(float) = lrintf;
    if (fused(0x1.000002p0f, 0x1.fffffep0f, -2.0f) != 0x1.fffffcp-24f || integer(2.5f) != 2)
        return 1;
    std::printf("%s %d\n", name.c_str(), values[0]);
    return 0;
}
