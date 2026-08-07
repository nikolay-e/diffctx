# Appendix B — runtime and memory telemetry (#168)

Source: per-instance `latency_breakdown` in `results/sweep_v2_local` checkpoints; ok rows only.

| cell | benchmark | n | total p50/p90/p99 ms | peak RSS p50/p99 MB | edges p50 (pre-cap p50) | dominant phases (share of summed phase time) |
|---|---|---:|---|---|---|---|
| ego_boltzmann | contextbench_verified | 492 | 2990/17938/88567 | 4765/21797 | 421195 (2078436) | graph_build 76%, discovery 12%, parse_discovered 6% |
| ego_boltzmann | polybench500 | 483 | 2256/22573/124236 | 3065/21587 | 331839 (857290) | graph_build 74%, discovery 11%, parse_discovered 8% |
| ego_boltzmann | swebench_verified | 500 | 3098/12225/17858 | 2135/4086 | 943533 (2751014) | graph_build 68%, parse_discovered 15%, tokenization 7% |
| ego_eps0 | contextbench_verified | 492 | 3035/18213/106870 | 9793/21276 | 421195 (2078436) | graph_build 77%, discovery 11%, parse_discovered 6% |
| ego_eps0 | polybench500 | 483 | 2341/23160/122930 | 3172/22040 | 331839 (857290) | graph_build 73%, discovery 11%, parse_discovered 8% |
| ego_eps0 | swebench_verified | 500 | 3152/12312/17525 | 2138/4153 | 943533 (2751014) | graph_build 68%, parse_discovered 15%, tokenization 7% |
| ego_nobonus | contextbench_verified | 492 | 3092/17539/97528 | 4595/20975 | 421195 (2078436) | graph_build 76%, discovery 12%, parse_discovered 6% |
| ego_nobonus | polybench500 | 483 | 2314/23104/121794 | 3268/20619 | 331839 (857290) | graph_build 73%, discovery 11%, parse_discovered 8% |
| ego_nobonus | swebench_verified | 500 | 3124/12457/17664 | 2157/4962 | 943533 (2751014) | graph_build 68%, parse_discovered 14%, tokenization 7% |
| ego_tau0 | contextbench_verified | 492 | 2999/18058/97374 | 3858/23064 | 421195 (2078436) | graph_build 76%, discovery 11%, parse_discovered 6% |
| ego_tau0 | polybench500 | 483 | 2341/22732/123451 | 3093/21568 | 331839 (857290) | graph_build 73%, discovery 11%, parse_discovered 8% |
| ego_tau0 | swebench_verified | 500 | 3173/12153/17774 | 2165/4446 | 943533 (2751014) | graph_build 68%, parse_discovered 15%, tokenization 7% |
