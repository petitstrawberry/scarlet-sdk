// C/POSIX calendar formatting using the SDK's pinned LLVM libc core.
#include <time.h>
#define LIBC_NAMESPACE __llvm_libc
#include "src/time/strftime_core/strftime_main.h"

extern "C" size_t strftime(char *output, size_t capacity, const char *format,
                           const struct tm *time) {
    namespace core = __llvm_libc::printf_core;
    core::WriteBuffer<core::WriteMode::FILL_BUFF_AND_DROP_OVERFLOW> buffer(
        output, capacity ? capacity - 1 : 0);
    core::Writer writer(buffer);
    __llvm_libc::strftime_core::Parser parser(format);
    int status = 0;
    for (auto section = parser.get_next_section(); !section.raw_string.empty();
         section = parser.get_next_section()) {
        if (section.has_conv && section.conv_name == 'Z') {
            if (time->tm_isdst >= 0 && time->tm_zone)
                status = writer.write(__llvm_libc::cpp::string_view(time->tm_zone));
        } else if (section.has_conv && section.conv_name == 'z') {
            if (time->tm_isdst >= 0) {
                long offset = time->tm_gmtoff / 60;
                unsigned long minutes = offset < 0 ? 0UL - static_cast<unsigned long>(offset) : offset;
                char zone[] = { offset < 0 ? '-' : '+',
                    static_cast<char>('0' + minutes / 600 % 10),
                    static_cast<char>('0' + minutes / 60 % 10),
                    static_cast<char>('0' + minutes % 60 / 10),
                    static_cast<char>('0' + minutes % 10) };
                status = writer.write(__llvm_libc::cpp::string_view(zone, sizeof(zone)));
            }
        } else if (section.has_conv) {
            status = __llvm_libc::strftime_core::convert(&writer, section, time);
        } else {
            status = writer.write(section.raw_string);
        }
        if (status < 0) break;
    }
    int count = status < 0 ? status : writer.get_chars_written();
    if (capacity) output[buffer.buff_cur] = '\0';
    return count < 0 || static_cast<size_t>(count) >= capacity ? 0 : count;
}
