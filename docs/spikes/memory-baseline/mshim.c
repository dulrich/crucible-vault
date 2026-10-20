// spike-only LD_PRELOAD shim: a thread reads commands from $MSHIM_FIFO ("stats" | "trim") and runs
// malloc_stats() (to stderr) / malloc_trim(0). Throwaway measurement aid, never shipped.
#define _GNU_SOURCE
#include <malloc.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
static void *loop(void *a) {
  const char *p = getenv("MSHIM_FIFO"); char buf[64];
  for (;;) { FILE *f = fopen(p, "r"); if (!f) return 0;
    while (fgets(buf, sizeof buf, f)) {
      if (!strncmp(buf, "stats", 5)) { fprintf(stderr, "MSHIM-STATS-BEGIN\n"); malloc_stats(); fprintf(stderr, "MSHIM-STATS-END\n"); }
      else if (!strncmp(buf, "trim", 4)) fprintf(stderr, "MSHIM-TRIM %d\n", malloc_trim(0)); }
    fclose(f); }
}
__attribute__((constructor)) static void init(void) {
  if (!getenv("MSHIM_FIFO")) return; pthread_t t; pthread_create(&t, 0, loop, 0); pthread_detach(t);
}
