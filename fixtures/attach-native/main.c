#include <stdio.h>
#include <unistd.h>
#ifdef __linux__
#include <sys/prctl.h>
#endif

int main(void) {
#ifdef __linux__
    /* Allow the test's sibling lldb-dap process to attach under Yama. */
    prctl(PR_SET_PTRACER, PR_SET_PTRACER_ANY, 0, 0, 0);
#endif
    for (;;) {
        puts("alive");
        fflush(stdout);
        usleep(50000);
    }
}
