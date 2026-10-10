#include <stdio.h>

struct point {
    int x;
    int y;
};

int add(int a, int b) {
    int sum = a + b;
    return sum;
}

int main(void) {
    struct point point = {3, 4};
    const char *greeting = "hello";
    int total = add(point.x, point.y); // ddbg:call-add
    printf("%s: %d\n", greeting, total);
    return 0;
}
