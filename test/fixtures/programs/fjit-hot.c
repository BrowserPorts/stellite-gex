// Loops the function JIT's hot tier takes while they run (`?fjit=hot`):
// tier-1 runs them first, the safepoint's samples make them hot, a helper
// Worker translates them, and the thread goes into the JIT function at its
// next sample there. Each round's sums must be the same whichever tier ran
// it, an indirect call's callee and a float loop among them.
#include <stdio.h>

__attribute__((noinline)) static long step(long a, long i) { return (a ^ i) * 31 + (i >> 3); }
static long (*volatile op)(long, long) = step;

int main(void) {
  long acc = 1;
  double d = 0.5, s = 0;
  for (int r = 0; r < 8; r++) {
    for (long i = 0; i < 2000000; i++) acc = op(acc, i);
    for (long i = 0; i < 1000000; i++) {
      d = d * 1.0000001 + 0.25 / (1.0 + d * d);
      s += d / (i + 1.0);
    }
    printf("round %d %ld %ld\n", r, acc & 0xffffff, (long)(s * 1000));
  }
  return 0;
}
