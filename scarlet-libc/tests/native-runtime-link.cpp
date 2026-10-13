// Installed SDK link check. Runtime behavior is verified separately in Scarlet.
#include <filesystem>
#include <cstdio>
#include <string>
#include <thread>
#include <vector>

int main() {
    std::filesystem::path path("/tmp/scarlet-sdk-check");
    std::string name = path.filename().string();
    std::vector<int> values { 1, 2, 3 };
    std::thread thread([&values] { values[0] = 4; });
    thread.join();
    std::printf("%s %d\n", name.c_str(), values[0]);
    return 0;
}
