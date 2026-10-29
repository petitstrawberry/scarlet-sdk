/* Use Rust libc's real stream-mode operation, including its rejection of
 * buffering requests for the current unbuffered FILE implementation. */
#include <stdio.h>

void setbuf(FILE *stream, char *buffer) {
    (void)setvbuf(stream, buffer, buffer ? _IOFBF : _IONBF, BUFSIZ);
}
