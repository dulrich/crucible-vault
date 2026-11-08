# Live daemon `cv jobs stats --since 7d` — pre-deploy baseline (round-23 Execution (e))

```
2026-11-07T23:21:09-06:00
cv 0.2.0 (70e36d708f2cd317fd1069c9c9d0efe9e92a0e48-dirty)
action:localize-on-edit  n=3074  partial=0
  total     p50 6ms  p95 11.0s  max 3m50s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 64ms  max 75ms
  key       p50 4ms  p95 10.9s  max 3m50s
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 2ms  p95 35ms  max 764ms
  residual  p50 0ms  p95 1ms  max 28ms
  blocker   key action:localize-on-edit  1h07m
  blocker   key action:clip-enrich  18m58s
  blocker   key action:track-blogs  3m46s
action:search-refresh  n=198  partial=0
  total     p50 5.1s  p95 16.0s  max 4m58s
  deferral  p50 0ms  p95 0ms  max 4m00s
  permit    p50 0ms  p95 0ms  max 774ms
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 5.1s  p95 15.5s  max 2m45s
  residual  p50 1ms  p95 3ms  max 29.5s
  blocker   deferred local  5m00s
  blocker   permit action:daily-brief,action:image-describe-backfill,action:link-scan-all  1.1s
  blocker   permit action:image-describe-backfill,action:link-scan-all,action:localize-vault-sweep  1.1s
action:link-scan  n=3315  partial=0
  total     p50 4ms  p95 1.7s  max 3m50s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 1.7s  max 1.9s
  key       p50 0ms  p95 0ms  max 3m50s
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 4ms  p95 21ms  max 383ms
  residual  p50 0ms  p95 2ms  max 356ms
  blocker   permit action:link-scan,action:link-scan,action:link-scan,action:localize-on-edit  7m34s
  blocker   key action:clip-enrich  7m28s
  blocker   permit action:intake-reconcile,action:link-scan,action:link-scan,action:localize-on-edit  4m14s
action:image-describe-backfill  n=124  partial=0
  total     p50 80ms  p95 50.3s  max 2m53s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 1.4s  max 3.0s
  key       p50 0ms  p95 0ms  max 1m39s
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 75ms  p95 40.5s  max 2m53s
  residual  p50 1ms  p95 3ms  max 62ms
  blocker   key action:image-describe-backfill  3m10s
  blocker   permit action:search-refresh,action:track-blogs,action:track-youtube  14.0s
  blocker   permit action:link-scan-all,action:search-refresh,action:track-youtube  4.6s
action:intake-enrich  n=67  partial=0
  total     p50 5ms  p95 1m18s  max 1m53s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 1.5s
  key       p50 0ms  p95 1m03s  max 1m41s
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 4ms  p95 20.6s  max 30.2s
  residual  p50 1ms  p95 1ms  max 4ms
  blocker   key action:intake-enrich  9m18s
  blocker   permit action:image-describe-backfill,action:search-refresh,action:track-youtube  2.3s
  blocker   permit action:image-describe-backfill,action:search-refresh,action:track-mail  147ms
action:clip-enrich  n=56  partial=0
  total     p50 8ms  p95 52.8s  max 4m27s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 0ms  p95 6.2s  max 3m30s
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 7ms  p95 40.0s  max 3m56s
  residual  p50 0ms  p95 1ms  max 2ms
  blocker   key action:clip-enrich  4m08s
  blocker   key action:localize-on-edit  905ms
  blocker   key action:clip-auto-file  29ms
action:track-mail  n=434  partial=0
  total     p50 78ms  p95 4.7s  max 3m19s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 1.4s
  key       p50 0ms  p95 4.6s  max 3m19s
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 76ms  p95 208ms  max 2.2s
  residual  p50 1ms  p95 2ms  max 143ms
  blocker   key action:clip-enrich  3m29s
  blocker   key action:track-blogs  3m16s
  blocker   key action:localize-vault-sweep  18.4s
action:track-blogs  n=40  partial=0
  total     p50 4.6s  p95 12.0s  max 3m19s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 1.2s  max 1.4s
  key       p50 491ms  p95 1.7s  max 3m15s
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 4.1s  p95 9.7s  max 12.0s
  residual  p50 1ms  p95 3ms  max 4ms
  blocker   key action:clip-enrich  3m29s
  blocker   key action:localize-vault-sweep  18.4s
  blocker   permit action:link-scan-all,action:search-refresh,action:track-youtube  4.3s
action:localize-vault-sweep  n=22  partial=0
  total     p50 531ms  p95 1.7s  max 3m15s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 354ms  max 357ms
  key       p50 0ms  p95 0ms  max 3m15s
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 523ms  p95 1.6s  max 1.7s
  residual  p50 1ms  p95 4ms  max 5ms
  blocker   key action:clip-enrich  3m15s
  blocker   permit action:daily-brief,action:image-describe-backfill,action:link-scan-all  1.1s
  blocker   key action:localize-on-edit  12ms
action:x-discover  n=66  partial=0
  total     p50 123ms  p95 26.5s  max 39.9s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 121ms  p95 26.5s  max 39.9s
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 2ms  p95 4ms  max 5ms
  residual  p50 0ms  p95 0ms  max 1ms
  blocker   key action:clip-enrich  2m39s
  blocker   key action:clip-auto-file  3.9s
  blocker   key action:localize-on-edit  2.9s
action:track-youtube  n=39  partial=0
  total     p50 1.6s  p95 6.5s  max 6.6s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 75ms  p95 5.4s  max 5.4s
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 1.3s  p95 2.0s  max 2.6s
  residual  p50 0ms  p95 2ms  max 54ms
  blocker   permit action:image-describe-backfill,action:search-refresh,action:track-blogs  26.6s
  blocker   permit action:link-scan-all,action:localize-vault-sweep,action:search-refresh  6.2s
  blocker   permit action:clip-enrich,action:image-describe-backfill,action:search-refresh  5.3s
action:daily-brief  n=7  partial=0
  total     p50 776ms  p95 30.8s  max 30.8s
  deferral  p50 0ms  p95 30.0s  max 30.0s
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 641ms  p95 856ms  max 856ms
  residual  p50 1ms  p95 3ms  max 3ms
  blocker   deferred frankfurter  1m30s
action:vault-tidy  n=7  partial=0
  total     p50 8.0s  p95 11.5s  max 11.5s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 1.2s  p95 1.4s  max 1.4s
  key       p50 6.6s  p95 11.5s  max 11.5s
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 37ms  p95 43ms  max 43ms
  residual  p50 0ms  p95 2ms  max 2ms
  blocker   key action:track-blogs  43.2s
  blocker   key action:localize-vault-sweep  9.8s
  blocker   permit action:link-scan-all,action:search-refresh,action:track-youtube  4.3s
action:refine-transcript  n=1  partial=0
  total     p50 30.4s  p95 30.4s  max 30.4s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 30.4s  p95 30.4s  max 30.4s
  residual  p50 0ms  p95 0ms  max 0ms
action:link-scan-all  n=7  partial=0
  total     p50 2.7s  p95 2.8s  max 2.8s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 2.7s  p95 2.8s  max 2.8s
  residual  p50 1ms  p95 3ms  max 3ms
action:intake-reconcile  n=1  partial=0
  total     p50 11.5s  p95 11.5s  max 11.5s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 11.5s  p95 11.5s  max 11.5s
  residual  p50 1ms  p95 1ms  max 1ms
action:clip-auto-file  n=59  partial=0
  total     p50 78ms  p95 149ms  max 288ms
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 1ms  p95 76ms  max 216ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 76ms  p95 112ms  max 116ms
  residual  p50 0ms  p95 1ms  max 1ms
  blocker   key action:clip-auto-file  439ms
  blocker   key action:clip-images  69ms
action:dupes-scan  n=1  partial=0
  total     p50 4.8s  p95 4.8s  max 4.8s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 4.8s  p95 4.8s  max 4.8s
  residual  p50 1ms  p95 1ms  max 1ms
action:clip-images  n=54  partial=0
  total     p50 10ms  p95 613ms  max 770ms
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 8ms  p95 611ms  max 768ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 2ms  p95 3ms  max 3ms
  residual  p50 1ms  p95 1ms  max 2ms
  blocker   key action:localize-on-edit  2.8s
  blocker   key action:link-scan  266ms
action:file-as-kind  n=50  partial=0
  total     p50 38ms  p95 136ms  max 249ms
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 5ms  p95 105ms  max 221ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 33ms  p95 43ms  max 54ms
  residual  p50 0ms  p95 1ms  max 1ms
  blocker   key action:clip-auto-file  638ms
  blocker   key action:file-as-kind  141ms
  blocker   key action:localize-on-edit  54ms
action:period.open  n=57  partial=0
  total     p50 5ms  p95 7ms  max 11ms
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 5ms  p95 7ms  max 11ms
  residual  p50 0ms  p95 1ms  max 1ms
action:lint-note  n=14  partial=0
  total     p50 5ms  p95 24ms  max 24ms
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 5ms  p95 23ms  max 23ms
  residual  p50 0ms  p95 1ms  max 1ms
action:capture.monthly-observation  n=1  partial=0
  total     p50 4ms  p95 4ms  max 4ms
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 4ms  p95 4ms  max 4ms
  residual  p50 0ms  p95 0ms  max 0ms
```
