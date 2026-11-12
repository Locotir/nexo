/* A small program for the debugger's source-level tests: written for nexo, no outside code. */
#include <stdio.h>

struct pt {
    int x;
    int y;
    char tag[8];
};

static int rows[4][4];
static const char *greeting = "hello grid";

int plot(struct pt *p, int c) {
    int idx = p->y * 4 + p->x;
    rows[p->y][p->x] = c; /* STORE */
    return idx;
}

int main(int argc, char **argv) {
    struct pt p = {1, 2, "ab"};
    int n = argc;
    char line[32];
    if (fgets(line, sizeof line, stdin))
        p.x = line[0] - '0';
    int got = plot(&p, 7);
    if (argc > 1) {
        int *bad = 0;
        *bad = got; /* CRASH */
    }
    printf("%s %d %d %d\n", greeting, got, n, rows[2][p.x]);
    return 0;
}
