# Scorecard — workflows-v1 (code-review-checkout rerun)

Driver: `bridge` · wire: n/a · model: qwen-local · index: n/a

Target: endpoint `spawned jesse-bridge 0.168.0`, model `qwen-local`

Harness: claude-code · runs per task: 3 (pass^3)

| Class | Pass rate | Mean latency | Mean tool calls |
|---|---|---|---|
| code | 1/1 (100%) | 138890 ms | 6.0 |
| **TOTAL** | **1/1 (100%)** | **138890 ms** | **6.0** |

## pass^3 per task

A task passes only when all 3 runs passed.

| Task | Class | pass^k | Runs passed | Failed assertion kinds |
|---|---|---|---|---|
| code-review-checkout | code | PASS | 3/3 |  |

## Latency

| Measure | p50 | p95 | n |
|---|---|---|---|
| submit to first streamed event | 24970 ms | 27161 ms | 3 |
| submit to first model token | 123890 ms | 138554 ms | 3 |
| submit to result | 138890 ms | 147779 ms | 3 |
