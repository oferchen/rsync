/* RSS-1.b: Generate 1M empty files for RSS measurement.
 * Creates directory structure and 1M files in /tmp/rss_1m/.
 * Compile: gcc -O2 -o /tmp/rss_gen_files rss_gen_files.c
 * Run:     /tmp/rss_gen_files
 */
#include <stdio.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>
#include <string.h>
#include <errno.h>

static void mkdirs(const char *path) {
    char tmp[256];
    char *p;
    snprintf(tmp, sizeof(tmp), "%s", path);
    for (p = tmp + 1; *p; p++) {
        if (*p == '/') {
            *p = '\0';
            mkdir(tmp, 0755);
            *p = '/';
        }
    }
    mkdir(tmp, 0755);
}

int main(void) {
    char path[256];
    char dir[256];
    int count = 0;
    int i, pkg, mod_n, dir_idx;

    /* Create 1000 directories: pkg_NNN/src/mod_NN */
    printf("Creating directories...\n");
    for (dir_idx = 0; dir_idx < 1000; dir_idx++) {
        pkg = dir_idx / 10;
        mod_n = dir_idx % 10;
        snprintf(dir, sizeof(dir),
                 "/tmp/rss_1m/workspace/pkg_%03d/src/mod_%02d",
                 pkg, mod_n);
        mkdirs(dir);
    }

    /* 1000 directories x 1000 files = 1M files */
    printf("Creating 1M files...\n");
    for (i = 0; i < 1000000; i++) {
        dir_idx = i % 1000;
        pkg = dir_idx / 10;
        mod_n = dir_idx % 10;
        snprintf(path, sizeof(path),
                 "/tmp/rss_1m/workspace/pkg_%03d/src/mod_%02d/item_%06d.rs",
                 pkg, mod_n, i);
        int fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
        if (fd >= 0) {
            close(fd);
            count++;
        } else if (count == 0) {
            /* Report first error for debugging */
            printf("Failed to create %s: %s\n", path, strerror(errno));
        }
    }
    printf("Created %d files\n", count);
    return 0;
}
