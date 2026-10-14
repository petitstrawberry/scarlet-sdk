// Installed SDK link check. Runtime behavior is verified separately in Scarlet.
#include <filesystem>
#include <cstdio>
#include <string>
#include <thread>
#include <vector>
#include <math.h>

int main() {
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
