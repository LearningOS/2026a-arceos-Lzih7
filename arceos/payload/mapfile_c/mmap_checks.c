/* Extra sys_map coverage. Build as /tmp/sys-map-checks/mapfile, install with
 * update_disk.sh, then run exercises/sys_map. The standard mapfile.c is kept
 * unchanged for the course acceptance test. */
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(expr) do { \
    if (!(expr)) { \
        printf("mmap checks FAILED at line %d\n", __LINE__); \
        return 1; \
    } \
} while (0)

int main(void)
{
    static unsigned char data[5000];
    for (size_t i = 0; i < sizeof(data); ++i)
        data[i] = (unsigned char)(i % 251);
    int fd = open("mmap_checks_file", O_CREAT | O_TRUNC | O_RDWR, 0600);
    CHECK(fd >= 0);
    for (size_t written = 0; written < sizeof(data); ) {
        /* Keep fixture writes below a disk block: the existing sys_write
         * forwards user pointers, while direct DMA expects kernel addresses. */
        size_t count = sizeof(data) - written;
        if (count > 128)
            count = 128;
        ssize_t n = write(fd, data + written, count);
        CHECK(n > 0);
        written += (size_t)n;
    }
    CHECK(close(fd) == 0);
    fd = open("mmap_checks_file", O_RDONLY);
    CHECK(fd >= 0);

    unsigned char *p = mmap(NULL, sizeof(data), PROT_READ, MAP_PRIVATE, fd, 0);
    CHECK(p != MAP_FAILED && (uintptr_t)p % 4096 == 0);
    for (size_t i = 0; i < sizeof(data); ++i) {
        if (p[i] != data[i])
            printf("mismatch at %zu: got %u expected %u\n", i, p[i], data[i]);
        CHECK(p[i] == data[i]);
    }
    CHECK(p[5000] == 0 && p[8191] == 0); /* Partial final page is zeroed. */

    unsigned char *q = mmap(NULL, 904, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 4096);
    CHECK(q != MAP_FAILED && q != p);
    CHECK(q[0] == data[4096] && q[903] == data[4999]);
    q[0] ^= 0xff;
    CHECK(p[4096] == data[4096]); /* Separate private copies. */
    unsigned char first = 0xff;
    CHECK(read(fd, &first, 1) == 1 && first == data[0]); /* mmap preserves cursor. */

    /* Read on a write-only descriptor fails after mapping: exercise rollback. */
    int wr = open("mmap_checks_file", O_WRONLY);
    CHECK(wr >= 0);
    CHECK(mmap((void *)0x10000000, 4096, PROT_READ, MAP_PRIVATE, wr, 0) == MAP_FAILED);
    CHECK(close(wr) == 0);
    unsigned char *a = mmap((void *)0x10000000, 8192, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(a == (void *)0x10000000); /* Failed mapping released the VA area. */
    CHECK(a[0] == 0 && a[8191] == 0);
    a[0] = 42;
    a[8191] = 24;
    CHECK(a[0] == 42 && a[8191] == 24);
    unsigned char *b = mmap(a, 4096, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(b != MAP_FAILED && b != a); /* Occupied hint must not replace a. */

    CHECK(mmap(NULL, 0, PROT_READ, MAP_PRIVATE, fd, 0) == MAP_FAILED);
    CHECK(mmap(NULL, 4096, PROT_READ, MAP_PRIVATE, -1, 0) == MAP_FAILED);
    CHECK(mmap(NULL, 4096, PROT_READ, MAP_PRIVATE, fd, 1) == MAP_FAILED);
    CHECK(mmap(NULL, 4096, PROT_READ, MAP_SHARED, fd, 0) == MAP_FAILED);
    CHECK(mmap(a, 4096, PROT_READ, MAP_PRIVATE | MAP_FIXED, fd, 0) == MAP_FAILED);
    CHECK(mmap(NULL, SIZE_MAX, PROT_READ, MAP_PRIVATE, fd, 0) == MAP_FAILED);
    CHECK(close(fd) == 0);
    CHECK(p[4096] == data[4096]); /* Mapping survives close. */
    printf("mmap checks pass\n");
    return 0;
}
