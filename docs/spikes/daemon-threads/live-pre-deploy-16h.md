# Live daemon `cv jobs stats --since 16h` — pre-deploy baseline (round-23 Execution (e))

```
2026-11-07T23:21:09-06:00
action:search-refresh  n=21  partial=0
  total     p50 5.9s  p95 13.7s  max 16.7s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 774ms
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 5.9s  p95 13.7s  max 15.9s
  residual  p50 2ms  p95 3ms  max 3ms
  blocker   permit action:daily-brief,action:link-scan-all,action:localize-vault-sweep  422ms
  blocker   permit action:daily-brief,action:image-describe-backfill,action:link-scan-all  352ms
action:intake-enrich  n=5  partial=0
  total     p50 12.7s  p95 54.8s  max 54.8s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 0ms  p95 37.7s  max 37.7s
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 12.7s  p95 30.2s  max 30.2s
  residual  p50 1ms  p95 1ms  max 1ms
  blocker   key action:intake-enrich  45.5s
action:localize-on-edit  n=470  partial=0
  total     p50 4ms  p95 11ms  max 12.5s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 3ms  p95 6ms  max 12.5s
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 1ms  p95 2ms  max 157ms
  residual  p50 0ms  p95 1ms  max 7ms
  blocker   key action:intake-enrich  30.3s
  blocker   key action:track-blogs  18.9s
  blocker   key action:link-scan  1.3s
action:image-describe-backfill  n=13  partial=0
  total     p50 168ms  p95 22.1s  max 22.1s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 1.6s  max 1.6s
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 68ms  p95 22.1s  max 22.1s
  residual  p50 1ms  p95 3ms  max 3ms
  blocker   permit action:search-refresh,action:track-blogs,action:track-youtube  1.5s
  blocker   permit action:link-scan-all,action:search-refresh,action:track-youtube  1.0s
  blocker   permit action:link-scan-all,action:localize-vault-sweep,action:search-refresh  130ms
action:clip-enrich  n=2  partial=0
  total     p50 18.0s  p95 26.7s  max 26.7s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 18.0s  p95 26.7s  max 26.7s
  residual  p50 0ms  p95 1ms  max 1ms
action:link-scan  n=472  partial=0
  total     p50 3ms  p95 7ms  max 12.5s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 2ms
  key       p50 0ms  p95 0ms  max 12.5s
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 3ms  p95 5ms  max 12ms
  residual  p50 1ms  p95 2ms  max 18ms
  blocker   key action:intake-enrich  30.2s
  blocker   key action:localize-on-edit  23ms
  blocker   permit action:link-scan,action:link-scan,action:link-scan,action:localize-on-edit  2ms
action:track-mail  n=42  partial=0
  total     p50 77ms  p95 4.2s  max 7.9s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 1.2s
  key       p50 0ms  p95 4.2s  max 6.5s
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 76ms  p95 123ms  max 188ms
  residual  p50 1ms  p95 2ms  max 6ms
  blocker   key action:track-blogs  17.8s
  blocker   key action:localize-vault-sweep  1.6s
  blocker   permit action:link-scan-all,action:search-refresh,action:track-youtube  944ms
action:track-blogs  n=4  partial=0
  total     p50 4.2s  p95 7.8s  max 7.8s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 1.2s  max 1.2s
  key       p50 0ms  p95 1.3s  max 1.3s
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 4.2s  p95 5.2s  max 5.2s
  residual  p50 1ms  p95 2ms  max 2ms
  blocker   key action:localize-vault-sweep  1.6s
  blocker   permit action:link-scan-all,action:search-refresh,action:track-youtube  944ms
  blocker   permit action:daily-brief,action:image-describe-backfill,action:link-scan-all  351ms
action:vault-tidy  n=1  partial=0
  total     p50 8.0s  p95 8.0s  max 8.0s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 1.2s  p95 1.2s  max 1.2s
  key       p50 6.6s  p95 6.6s  max 6.6s
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 33ms  p95 33ms  max 33ms
  residual  p50 1ms  p95 1ms  max 1ms
  blocker   key action:track-blogs  5.2s
  blocker   key action:localize-vault-sweep  1.1s
  blocker   permit action:link-scan-all,action:search-refresh,action:track-youtube  944ms
action:track-youtube  n=4  partial=0
  total     p50 1.2s  p95 3.0s  max 3.0s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 66ms  p95 1.4s  max 1.4s
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 1.1s  p95 1.6s  max 1.6s
  residual  p50 0ms  p95 1ms  max 1ms
  blocker   permit action:link-scan-all,action:localize-vault-sweep,action:search-refresh  666ms
  blocker   permit action:daily-brief,action:link-scan-all,action:localize-vault-sweep  422ms
  blocker   permit action:daily-brief,action:image-describe-backfill,action:link-scan-all  349ms
action:link-scan-all  n=1  partial=0
  total     p50 2.6s  p95 2.6s  max 2.6s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 2.6s  p95 2.6s  max 2.6s
  residual  p50 1ms  p95 1ms  max 1ms
action:localize-vault-sweep  n=2  partial=0
  total     p50 518ms  p95 1.4s  max 1.4s
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 354ms  max 354ms
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 516ms  p95 1.0s  max 1.0s
  residual  p50 0ms  p95 2ms  max 2ms
  blocker   permit action:daily-brief,action:image-describe-backfill,action:link-scan-all  354ms
action:daily-brief  n=1  partial=0
  total     p50 776ms  p95 776ms  max 776ms
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 775ms  p95 775ms  max 775ms
  residual  p50 1ms  p95 1ms  max 1ms
action:x-discover  n=2  partial=0
  total     p50 108ms  p95 114ms  max 114ms
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 107ms  p95 113ms  max 113ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 1ms  p95 1ms  max 1ms
  residual  p50 0ms  p95 0ms  max 0ms
  blocker   key action:clip-auto-file  145ms
  blocker   key action:file-as-kind  64ms
  blocker   key action:link-scan  8ms
action:clip-auto-file  n=2  partial=0
  total     p50 68ms  p95 78ms  max 78ms
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 0ms  p95 1ms  max 1ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 68ms  p95 77ms  max 77ms
  residual  p50 0ms  p95 0ms  max 0ms
  blocker   key action:clip-images  1ms
action:file-as-kind  n=2  partial=0
  total     p50 34ms  p95 39ms  max 39ms
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 3ms  p95 5ms  max 5ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 29ms  p95 35ms  max 35ms
  residual  p50 0ms  p95 1ms  max 1ms
  blocker   key action:clip-auto-file  8ms
action:lint-note  n=5  partial=0
  total     p50 4ms  p95 6ms  max 6ms
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 3ms  p95 6ms  max 6ms
  residual  p50 0ms  p95 1ms  max 1ms
action:clip-images  n=2  partial=0
  total     p50 5ms  p95 7ms  max 7ms
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 4ms  p95 6ms  max 6ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 0ms  p95 1ms  max 1ms
  residual  p50 0ms  p95 1ms  max 1ms
  blocker   key action:link-scan  8ms
  blocker   key action:localize-on-edit  2ms
action:period.open  n=2  partial=0
  total     p50 2ms  p95 3ms  max 3ms
  deferral  p50 0ms  p95 0ms  max 0ms
  permit    p50 0ms  p95 0ms  max 0ms
  key       p50 0ms  p95 0ms  max 0ms
  breaker   p50 0ms  p95 0ms  max 0ms
  run       p50 1ms  p95 3ms  max 3ms
  residual  p50 0ms  p95 1ms  max 1ms
```
