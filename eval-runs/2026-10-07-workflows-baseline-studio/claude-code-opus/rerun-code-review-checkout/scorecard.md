# Scorecard — workflows-v1 (code-review-checkout rerun)

Driver: `bridge` · wire: n/a · model: opus · index: n/a

Target: endpoint `spawned jesse-bridge 0.168.0`, model `opus`

Harness: claude-code · runs per task: 3 (pass^3)

| Class | Pass rate | Mean latency | Mean tool calls |
|---|---|---|---|
| code | 1/1 (100%) | 63398 ms | 7.0 |
| **TOTAL** | **1/1 (100%)** | **63398 ms** | **7.0** |

## pass^3 per task

A task passes only when all 3 runs passed.

| Task | Class | pass^k | Runs passed | Failed assertion kinds |
|---|---|---|---|---|
| code-review-checkout | code | PASS | 3/3 |  |

## Latency

| Measure | p50 | p95 | n |
|---|---|---|---|
| submit to first streamed event | 32925 ms | 37099 ms | 3 |
| submit to first model token | 53998 ms | 88778 ms | 3 |
| submit to result | 63398 ms | 98111 ms | 3 |
